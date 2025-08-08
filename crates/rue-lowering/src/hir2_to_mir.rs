//! HIR2 to MIR lowering
//!
//! This module converts HIR2 (instruction-based HIR) to MIR (Mid-level IR) in SSA form.
//! Unlike the tree-based HIR to MIR lowering, this processes instructions sequentially,
//! making it much simpler since instructions are already in execution order.
//!
//! ## Key Advantages of HIR2 to MIR Lowering
//!
//! 1. **Sequential Processing**: Instructions are processed in a single pass without
//!    recursive tree traversal, significantly simplifying the implementation.
//!
//! 2. **Cache Efficiency**: The instruction stream format is cache-friendly, reducing
//!    memory access overhead during lowering.
//!
//! 3. **Dependency Order**: HIR2 instructions are already in dependency order, eliminating
//!    the need for complex dependency analysis during lowering.
//!
//! 4. **Simplified State Management**: The linear nature means simpler tracking of
//!    instruction-to-temporary mappings and variable scopes.
//!
//! ## Implementation Overview
//!
//! The lowering process:
//! 1. Collect all instructions from function blocks
//! 2. Process each instruction in order, generating MIR statements
//! 3. Map HIR2 instruction indices to MIR temporaries
//! 4. Handle operators by extracting them from extra_data storage
//! 5. Generate basic blocks with appropriate terminators
//!
//! The key insight is that HIR2 instructions are already linearized and in the correct
//! dependency order, so we can process them in a single pass without recursive traversal.

use rue_ir::hir2::{BinOp, FunctionData, Hir, InstData, InstIndex, InstTag, StringIndex, UnaryOp};
use rue_ir::mir::{
    BasicBlock, BlockId, CallKind, FunctionSignature, MirBinOp, MirConst, MirFunction, MirProgram,
    MirStatement, MirTerminator, MirUnaryOp, MirValue, Temp,
};
use rue_ir::types::{RueType, TypeContext};
use std::collections::HashMap;
use tracing::{debug, trace};

/// Lowering context for converting HIR2 instructions to MIR
pub struct Hir2ToMirLowerer {
    /// Counter for generating unique temporaries
    temp_counter: u32,
    /// Counter for generating unique blocks
    block_counter: u32,
    /// Current basic block being built
    current_block: Option<BasicBlock>,
    /// All completed blocks
    blocks: Vec<BasicBlock>,
    /// Mapping from variable names to their current temporary
    variables: HashMap<String, Temp>,
    /// Type information for temporaries
    temp_types: HashMap<Temp, RueType>,
    /// Mapping from HIR2 instruction indices to MIR temporaries
    inst_to_temp: HashMap<InstIndex, Temp>,
    /// The HIR2 being processed (for accessing strings, types, etc.)
    hir: *const Hir,
}

impl Hir2ToMirLowerer {
    /// Create a new HIR2 to MIR lowerer
    fn new(hir: &Hir) -> Self {
        Self {
            temp_counter: 0,
            block_counter: 0,
            current_block: None,
            blocks: Vec::new(),
            variables: HashMap::new(),
            temp_types: HashMap::new(),
            inst_to_temp: HashMap::new(),
            hir: hir as *const Hir,
        }
    }

    /// Get a reference to the HIR2 being processed
    fn hir(&self) -> &Hir {
        unsafe { &*self.hir }
    }

    /// Generate a fresh temporary with type registration
    fn fresh_temp(&mut self, ty: RueType) -> Temp {
        let temp = Temp(self.temp_counter);
        self.temp_counter += 1;
        self.temp_types.insert(temp, ty);
        temp
    }

    /// Generate a fresh block ID
    fn fresh_block(&mut self) -> BlockId {
        let id = BlockId(self.block_counter);
        self.block_counter += 1;
        id
    }

    /// Start a new basic block
    fn start_block(&mut self, id: BlockId, params: Vec<(Temp, RueType)>) {
        if let Some(block) = self.current_block.take() {
            self.blocks.push(block);
        }
        self.current_block = Some(BasicBlock {
            id,
            params,
            statements: Vec::new(),
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            }, // Placeholder
        });
    }

    /// Add a statement to the current block
    fn add_statement(&mut self, stmt: MirStatement) {
        if let Some(block) = &mut self.current_block {
            block.statements.push(stmt);
        }
    }

    /// Set the terminator for the current block and finish it
    fn finish_block(&mut self, terminator: MirTerminator) {
        if let Some(mut block) = self.current_block.take() {
            block.terminator = terminator;
            self.blocks.push(block);
        }
    }

    /// Create a new temporary and assign a constant value to it
    fn emit_const(&mut self, constant: MirConst, span: Option<rue_lexer::Span>) -> Temp {
        let ty = constant.ty();
        let temp = self.fresh_temp(ty);
        self.add_statement(MirStatement::Assign {
            dest: temp,
            value: MirValue::Const(constant),
            span,
        });
        temp
    }

    /// Create a new temporary and assign a binary operation to it
    fn emit_binop(
        &mut self,
        op: MirBinOp,
        lhs: Temp,
        rhs: Temp,
        result_ty: RueType,
        span: Option<rue_lexer::Span>,
    ) -> Temp {
        let temp = self.fresh_temp(result_ty);
        self.add_statement(MirStatement::Assign {
            dest: temp,
            value: MirValue::BinaryOp { op, lhs, rhs },
            span,
        });
        temp
    }

    /// Create a new temporary and assign a unary operation to it
    fn emit_unop(
        &mut self,
        op: MirUnaryOp,
        operand: Temp,
        result_ty: RueType,
        span: Option<rue_lexer::Span>,
    ) -> Temp {
        let temp = self.fresh_temp(result_ty);
        self.add_statement(MirStatement::Assign {
            dest: temp,
            value: MirValue::UnaryOp { op, operand },
            span,
        });
        temp
    }

    /// Create a new temporary and assign a function call to it
    fn emit_call(
        &mut self,
        func: String,
        args: Vec<Temp>,
        kind: CallKind,
        return_ty: RueType,
        span: Option<rue_lexer::Span>,
    ) -> Temp {
        let temp = self.fresh_temp(return_ty);
        self.add_statement(MirStatement::Assign {
            dest: temp,
            value: MirValue::Call { func, args, kind },
            span,
        });
        temp
    }

    /// Process a single HIR2 instruction and return the resulting temporary (if any)
    fn process_instruction(
        &mut self,
        index: InstIndex,
        tag: InstTag,
        data: InstData,
    ) -> Option<Temp> {
        let span = Some(self.hir().get_span(index));
        let ty = self.hir().get_type(index);

        trace!(
            "Processing instruction {:?}: {:?} with data {:?}",
            index, tag, data
        );

        match tag {
            InstTag::Literal => {
                // Reconstruct the 64-bit literal value
                let value = ((data.rhs as u64) << 32) | (data.lhs as u64);
                let const_val = MirConst::Int64(value as i64);
                let temp = self.emit_const(const_val, span);
                Some(temp)
            }

            InstTag::Load => {
                // Load a variable by name
                let name_idx = StringIndex::from_raw(data.lhs);
                let name = self.hir().strings.get(name_idx);
                if let Some(&temp) = self.variables.get(name) {
                    // Variable already has a temp, just return it
                    Some(temp)
                } else {
                    // This should not happen in well-formed HIR2
                    panic!("Undefined variable: {}", name);
                }
            }

            InstTag::Binary => {
                // Binary operation
                let lhs_inst = InstIndex::new(data.lhs).expect("Invalid lhs instruction");
                let rhs_inst = InstIndex::new(data.rhs).expect("Invalid rhs instruction");

                let lhs_temp = self.inst_to_temp[&lhs_inst];
                let rhs_temp = self.inst_to_temp[&rhs_inst];

                // Get the operator from extra_data
                let hir_op = if index.as_usize() < self.hir().extra_data.len() {
                    BinOp::from_u32(self.hir().extra_data[index.as_usize()])
                } else {
                    BinOp::Add // Default fallback
                };

                let mir_op = match hir_op {
                    BinOp::Add => MirBinOp::Add,
                    BinOp::Sub => MirBinOp::Sub,
                    BinOp::Mul => MirBinOp::Mul,
                    BinOp::Div => MirBinOp::Div,
                    BinOp::Mod => MirBinOp::Mod,
                    BinOp::Eq => MirBinOp::Eq,
                    BinOp::Ne => MirBinOp::Ne,
                    BinOp::Lt => MirBinOp::Lt,
                    BinOp::Le => MirBinOp::Le,
                    BinOp::Gt => MirBinOp::Gt,
                    BinOp::Ge => MirBinOp::Ge,
                };

                let result_ty = ty.cloned().unwrap_or(RueType::I64);
                let temp = self.emit_binop(mir_op, lhs_temp, rhs_temp, result_ty, span);
                Some(temp)
            }

            InstTag::Unary => {
                // Unary operation
                let operand_inst = InstIndex::new(data.lhs).expect("Invalid operand instruction");
                let operand_temp = self.inst_to_temp[&operand_inst];

                // Get the operator from extra_data
                let hir_op = if index.as_usize() < self.hir().extra_data.len() {
                    UnaryOp::from_u32(self.hir().extra_data[index.as_usize()])
                } else {
                    UnaryOp::Neg // Default fallback
                };

                let mir_op = match hir_op {
                    UnaryOp::Neg => MirUnaryOp::Neg,
                };

                let result_ty = ty.cloned().unwrap_or(RueType::I64);
                let temp = self.emit_unop(mir_op, operand_temp, result_ty, span);
                Some(temp)
            }

            InstTag::Call => {
                // Function call
                let func_name_idx = StringIndex::from_raw(data.lhs);
                let func_name = self.hir().strings.get(func_name_idx).to_string();

                // Extract arguments from extra_data
                let args_start = data.rhs as usize;
                let mut args = Vec::new();

                // The first element in extra_data is the argument count
                if args_start < self.hir().extra_data.len() {
                    let arg_count = self.hir().extra_data[args_start] as usize;

                    // Extract argument instruction indices and convert to temps
                    for i in 1..=arg_count {
                        let extra_idx = args_start + i;
                        if extra_idx < self.hir().extra_data.len() {
                            let arg_inst_raw = self.hir().extra_data[extra_idx];
                            if let Some(arg_inst) = InstIndex::new(arg_inst_raw) {
                                if let Some(&arg_temp) = self.inst_to_temp.get(&arg_inst) {
                                    args.push(arg_temp);
                                } else {
                                    // This shouldn't happen in well-formed HIR2
                                    panic!(
                                        "Argument instruction {} not found in temp mapping",
                                        arg_inst_raw
                                    );
                                }
                            }
                        }
                    }
                }

                let return_ty = ty.cloned().unwrap_or(RueType::I64);
                let kind = CallKind::Impure; // Conservative assumption
                let temp = self.emit_call(func_name, args, kind, return_ty, span);
                Some(temp)
            }

            InstTag::Let => {
                // Variable declaration/assignment
                let name_idx = StringIndex::from_raw(data.lhs);
                let name = self.hir().strings.get(name_idx).to_string();

                if data.rhs != 0 {
                    // Has initializer
                    let init_inst =
                        InstIndex::new(data.rhs).expect("Invalid initializer instruction");
                    let init_temp = self.inst_to_temp[&init_inst];
                    self.variables.insert(name, init_temp);
                } else {
                    // No initializer - this shouldn't happen in well-formed HIR2
                    panic!("Let without initializer: {}", name);
                }
                None // Let doesn't produce a value
            }

            InstTag::Return => {
                // Return statement
                if data.lhs != 0 {
                    let value_inst =
                        InstIndex::new(data.lhs).expect("Invalid return value instruction");
                    let value_temp = self.inst_to_temp[&value_inst];
                    self.finish_block(MirTerminator::Return {
                        value: Some(value_temp),
                        span,
                    });
                } else {
                    self.finish_block(MirTerminator::Return { value: None, span });
                }
                None
            }

            // Other instruction types would be handled here
            _ => {
                debug!("Unhandled instruction type: {:?}", tag);
                None
            }
        }
    }

    /// Lower a single function from HIR2 to MIR
    fn lower_function(&mut self, func_data: &FunctionData, _func_index: usize) -> MirFunction {
        debug!(
            "Lowering function: {}",
            self.hir().strings.get(func_data.name)
        );

        // Reset state for new function
        self.temp_counter = 0;
        self.block_counter = 0;
        self.current_block = None;
        self.blocks.clear();
        self.variables.clear();
        self.temp_types.clear();
        self.inst_to_temp.clear();

        // Create entry block
        let entry_block_id = self.fresh_block();

        // Create parameter temporaries
        let block_params = Vec::new();
        // TODO: Extract parameter information from function data
        // For now, we'll create a simple entry block

        self.start_block(entry_block_id, block_params);

        // Process the function body block
        let body_block = func_data.body_block;

        // Collect the instructions first to avoid borrowing issues
        let mut instructions = Vec::new();
        self.hir().walk_block(body_block, |inst_index, tag, data| {
            instructions.push((inst_index, tag, data));
        });

        // Now process the instructions
        for (inst_index, tag, data) in instructions {
            if let Some(temp) = self.process_instruction(inst_index, tag, data) {
                self.inst_to_temp.insert(inst_index, temp);
            }
        }

        // Finish any remaining block
        if self.current_block.is_some() {
            self.finish_block(MirTerminator::Return {
                value: None,
                span: None,
            });
        }

        // Build the function
        MirFunction {
            name: self.hir().strings.get(func_data.name).to_string(),
            params: Vec::new(), // TODO: Extract from function data
            return_type: func_data.return_type.clone().unwrap_or(RueType::I64),
            blocks: std::mem::take(&mut self.blocks),
            entry_block: entry_block_id,
            temp_types: std::mem::take(&mut self.temp_types),
            span: rue_lexer::Span { start: 0, end: 0 }, // TODO: Get actual span
        }
    }
}

/// Add builtin function signatures to the function registry
fn add_builtin_function_signatures(function_signatures: &mut HashMap<String, FunctionSignature>) {
    // Add print function
    function_signatures.insert(
        "print".to_string(),
        FunctionSignature {
            param_types: vec![RueType::I64],
            return_type: RueType::I64,
        },
    );

    // Add other builtin functions as needed
}

/// Main entry point: Lower HIR2 to MIR
pub fn lower_hir2_to_mir(hir: &Hir, type_context: TypeContext) -> MirProgram {
    debug!("Starting HIR2 to MIR lowering");

    let mut functions = Vec::new();
    let mut function_signatures = HashMap::new();

    // First pass: collect all function signatures (both user-defined and builtin)
    add_builtin_function_signatures(&mut function_signatures);

    // Add user-defined function signatures
    for func_data in hir.functions.iter() {
        let sig = FunctionSignature {
            param_types: Vec::new(), // TODO: Extract parameter types from function data
            return_type: func_data.return_type.clone().unwrap_or(RueType::I64),
        };
        function_signatures.insert(hir.strings.get(func_data.name).to_string(), sig);
    }

    // Second pass: lower functions to MIR
    let mut lowerer = Hir2ToMirLowerer::new(hir);
    for (i, func_data) in hir.functions.iter().enumerate() {
        let mir_func = lowerer.lower_function(func_data, i);
        functions.push(mir_func);
    }

    debug!(
        "HIR2 to MIR lowering complete. Generated {} functions",
        functions.len()
    );

    MirProgram {
        functions,
        function_signatures,
        type_context,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_ir::hir2::{InstData, InstTag};
    use rue_ir::types::RueType;

    #[test]
    fn test_basic_lowering() {
        let mut hir = Hir::new();

        // Create a simple function: fn main() -> i32 { return 42; }
        let main_name = hir.strings.intern("main");

        // Add instructions: literal 42, then return it
        let literal_inst = hir.add_instruction(
            InstTag::Literal,
            InstData::literal(42),
            Some(RueType::I64),
            rue_lexer::Span { start: 0, end: 2 },
        );

        hir.add_instruction(
            InstTag::Return,
            InstData {
                lhs: literal_inst.raw(),
                rhs: 0,
            },
            None,
            rue_lexer::Span { start: 3, end: 12 },
        );

        // Create a block for the function body
        let body_block = hir.add_block(1, 2); // Instructions 1-2

        // Create the main function
        let func_data = FunctionData {
            name: main_name,
            param_count: 0,
            return_type: Some(RueType::I64),
            body_block,
        };
        hir.functions.push(func_data);

        // Lower to MIR
        let type_context = TypeContext::new();
        let mir = lower_hir2_to_mir(&hir, type_context);

        assert_eq!(mir.functions.len(), 1);
        assert_eq!(mir.functions[0].name, "main");
        assert_eq!(mir.functions[0].return_type, RueType::I64);
    }
}
