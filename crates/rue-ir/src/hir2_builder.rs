//! HIR2 Builder - Provides a convenient API for constructing HIR2 instructions
//!
//! The builder pattern allows for incremental construction of HIR2 while maintaining
//! invariants and providing a clean API for the type checker.

use crate::hir2::*;
use crate::types::RueType;
use rue_lexer::Span;
use std::collections::HashMap;

/// Scope information for variable tracking
#[derive(Debug, Clone)]
struct Scope {
    variables: HashMap<String, (InstIndex, RueType)>,
}

impl Scope {
    fn new() -> Self {
        Self {
            variables: HashMap::new(),
        }
    }

    fn declare_var(&mut self, name: String, inst: InstIndex, ty: RueType) {
        self.variables.insert(name, (inst, ty));
    }

    fn lookup_var(&self, name: &str) -> Option<(InstIndex, RueType)> {
        self.variables.get(name).cloned()
    }
}

/// Builder for constructing HIR2 with proper scoping and validation
pub struct HirBuilder {
    hir: Hir,
    current_block: Option<BlockIndex>,
    block_start: Option<u32>, // Track start of current block for instruction counting
    scope_stack: Vec<Scope>,
    current_span: Span,
}

impl HirBuilder {
    /// Create a new HIR builder
    pub fn new() -> Self {
        Self {
            hir: Hir::new(),
            current_block: None,
            block_start: None,
            scope_stack: vec![Scope::new()], // Global scope
            current_span: Span { start: 0, end: 0 },
        }
    }

    /// Set the current source span for error reporting
    pub fn set_span(&mut self, span: Span) {
        self.current_span = span;
    }

    /// Get the next instruction index that will be allocated
    fn next_inst_index(&self) -> u32 {
        self.hir.instructions.len() as u32
    }

    /// Emit a literal instruction
    pub fn emit_literal(&mut self, value: u64, ty: RueType) -> InstIndex {
        let data = InstData::literal(value);
        self.hir
            .add_instruction(InstTag::Literal, data, Some(ty), self.current_span)
    }

    /// Emit a load instruction (variable reference)
    pub fn emit_load(&mut self, name: &str) -> Result<InstIndex, String> {
        // Look up variable in scope stack (from innermost to outermost)
        for scope in self.scope_stack.iter().rev() {
            if let Some((_, var_ty)) = scope.lookup_var(name) {
                let name_idx = self.hir.strings.intern(name);
                let data = InstData::string(name_idx);
                return Ok(self.hir.add_instruction(
                    InstTag::Load,
                    data,
                    Some(var_ty.clone()),
                    self.current_span,
                ));
            }
        }
        Err(format!("Undefined variable: {}", name))
    }

    /// Emit a binary operation instruction
    pub fn emit_binary(
        &mut self,
        lhs: InstIndex,
        rhs: InstIndex,
        op: BinOp,
        result_ty: RueType,
    ) -> InstIndex {
        let data = InstData {
            lhs: lhs.raw(),
            rhs: rhs.raw(),
        };

        let inst =
            self.hir
                .add_instruction(InstTag::Binary, data, Some(result_ty), self.current_span);

        // Store the operator in extra_data at the instruction's position
        // This is a simplification - real implementation would be more sophisticated
        if self.hir.extra_data.len() <= inst.as_usize() {
            self.hir.extra_data.resize(inst.as_usize() + 1, 0);
        }
        if inst.as_usize() < self.hir.extra_data.len() {
            self.hir.extra_data[inst.as_usize()] = op as u32;
        }

        inst
    }

    /// Emit a unary operation instruction
    pub fn emit_unary(&mut self, operand: InstIndex, op: UnaryOp, result_ty: RueType) -> InstIndex {
        let data = InstData {
            lhs: operand.raw(),
            rhs: 0,
        };

        let inst =
            self.hir
                .add_instruction(InstTag::Unary, data, Some(result_ty), self.current_span);

        // Store operator in extra_data
        if self.hir.extra_data.len() <= inst.as_usize() {
            self.hir.extra_data.resize(inst.as_usize() + 1, 0);
        }
        if inst.as_usize() < self.hir.extra_data.len() {
            self.hir.extra_data[inst.as_usize()] = op as u32;
        }

        inst
    }

    /// Emit a function call instruction
    pub fn emit_call(
        &mut self,
        func_name: &str,
        args: Vec<InstIndex>,
        return_ty: RueType,
    ) -> InstIndex {
        let func_name_idx = self.hir.strings.intern(func_name);

        // Store arguments in extra_data with count first: [arg_count, arg1, arg2, ...]
        let mut arg_data = Vec::with_capacity(args.len() + 1);
        arg_data.push(args.len() as u32); // Store argument count first
        arg_data.extend(args.iter().map(|inst| inst.raw()));
        let args_start = self.hir.add_extra_data(&arg_data);

        let data = InstData {
            lhs: func_name_idx.raw(),
            rhs: args_start,
        };

        self.hir
            .add_instruction(InstTag::Call, data, Some(return_ty), self.current_span)
    }

    /// Emit a let statement (variable declaration)
    pub fn emit_let(&mut self, name: &str, init: InstIndex, var_ty: RueType) -> InstIndex {
        let name_idx = self.hir.strings.intern(name);
        let data = InstData {
            lhs: name_idx.raw(),
            rhs: init.raw(),
        };

        let inst = self
            .hir
            .add_instruction(InstTag::Let, data, None, self.current_span);

        // Add variable to current scope
        if let Some(scope) = self.scope_stack.last_mut() {
            scope.declare_var(name.to_string(), inst, var_ty);
        }

        inst
    }

    /// Emit an assign statement
    pub fn emit_assign(&mut self, name: &str, value: InstIndex) -> Result<InstIndex, String> {
        // Verify variable exists in scope
        let mut found = false;
        for scope in self.scope_stack.iter().rev() {
            if scope.lookup_var(name).is_some() {
                found = true;
                break;
            }
        }

        if !found {
            return Err(format!("Cannot assign to undefined variable: {}", name));
        }

        let name_idx = self.hir.strings.intern(name);
        let data = InstData {
            lhs: name_idx.raw(),
            rhs: value.raw(),
        };

        Ok(self
            .hir
            .add_instruction(InstTag::Assign, data, None, self.current_span))
    }

    /// Emit a return statement
    pub fn emit_return(&mut self, value: Option<InstIndex>) -> InstIndex {
        let data = InstData {
            lhs: value.map(|i| i.raw()).unwrap_or(0),
            rhs: 0,
        };

        self.hir
            .add_instruction(InstTag::Return, data, None, self.current_span)
    }

    /// Start a new block
    pub fn start_block(&mut self) -> BlockIndex {
        let start_inst = self.next_inst_index();
        self.block_start = Some(start_inst);

        // Push new scope
        self.scope_stack.push(Scope::new());

        // We'll create the actual block when we end it
        BlockIndex::new(0).unwrap_or(BlockIndex::new(1).unwrap()) // Placeholder - will be fixed in end_block
    }

    /// End the current block
    pub fn end_block(&mut self) -> BlockIndex {
        let start_inst = self.block_start.take().unwrap_or(self.next_inst_index());
        let current_inst = self.next_inst_index();
        let inst_count = current_inst.saturating_sub(start_inst);

        // Pop scope
        self.scope_stack.pop();

        // Create the block
        let block_idx = self.hir.add_block(start_inst, inst_count);
        self.current_block = Some(block_idx);

        block_idx
    }

    /// Emit a while loop
    pub fn emit_while(&mut self, cond: InstIndex, body_block: BlockIndex) -> InstIndex {
        let data = InstData {
            lhs: cond.raw(),
            rhs: body_block.raw(),
        };

        self.hir
            .add_instruction(InstTag::While, data, None, self.current_span)
    }

    /// Emit an if expression
    pub fn emit_if(
        &mut self,
        cond: InstIndex,
        then_block: BlockIndex,
        else_block: Option<BlockIndex>,
        result_ty: Option<RueType>,
    ) -> InstIndex {
        // Store condition and blocks in extra_data
        let mut extra = vec![cond.raw(), then_block.raw()];
        if let Some(else_blk) = else_block {
            extra.push(else_blk.raw());
        } else {
            extra.push(0); // No else block
        }

        let extra_start = self.hir.add_extra_data(&extra);

        let data = InstData {
            lhs: extra_start,
            rhs: 0,
        };

        self.hir
            .add_instruction(InstTag::If, data, result_ty, self.current_span)
    }

    /// Create a function
    pub fn create_function(
        &mut self,
        name: &str,
        params: Vec<(String, RueType)>,
        return_type: RueType,
        body_block: BlockIndex,
    ) -> Result<InstIndex, String> {
        let name_idx = self.hir.strings.intern(name);

        let func_data = FunctionData {
            name: name_idx,
            param_count: params.len() as u32,
            return_type: Some(return_type),
            body_block,
        };

        let func_idx = self.hir.add_function(func_data);

        // If this is the main function, mark it
        if name == "main" {
            self.hir.main_function = Some(func_idx);
        }

        Ok(func_idx)
    }

    /// Get the current variable type by name
    pub fn get_variable_type(&self, name: &str) -> Option<RueType> {
        for scope in self.scope_stack.iter().rev() {
            if let Some((_, ty)) = scope.lookup_var(name) {
                return Some(ty);
            }
        }
        None
    }

    /// Get the type of an instruction
    pub fn get_instruction_type(&self, inst: InstIndex) -> Option<&RueType> {
        self.hir.get_type(inst)
    }

    /// Finish building and return the completed HIR
    pub fn finish(self) -> Hir {
        self.hir
    }
}

impl Default for HirBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RueType;

    #[test]
    fn test_builder_literals() {
        let mut builder = HirBuilder::new();

        let int_lit = builder.emit_literal(42, RueType::I32);
        let bool_lit = builder.emit_literal(1, RueType::Bool);

        let hir = builder.finish();

        assert_eq!(hir.instructions.tag(int_lit), InstTag::Literal);
        assert_eq!(hir.get_type(int_lit), Some(&RueType::I32));

        assert_eq!(hir.instructions.tag(bool_lit), InstTag::Literal);
        assert_eq!(hir.get_type(bool_lit), Some(&RueType::Bool));
    }

    #[test]
    fn test_builder_variables() {
        let mut builder = HirBuilder::new();

        // Create a literal
        let value = builder.emit_literal(42, RueType::I32);

        // Declare a variable
        let let_stmt = builder.emit_let("x", value, RueType::I32);

        // Load the variable
        let load_x = builder.emit_load("x").unwrap();

        let hir = builder.finish();

        assert_eq!(hir.instructions.tag(let_stmt), InstTag::Let);
        assert_eq!(hir.instructions.tag(load_x), InstTag::Load);
        assert_eq!(hir.get_type(load_x), Some(&RueType::I32));
    }

    #[test]
    fn test_builder_binary_ops() {
        let mut builder = HirBuilder::new();

        let left = builder.emit_literal(10, RueType::I32);
        let right = builder.emit_literal(5, RueType::I32);

        let add = builder.emit_binary(left, right, BinOp::Add, RueType::I32);

        let hir = builder.finish();

        assert_eq!(hir.instructions.tag(add), InstTag::Binary);
        assert_eq!(hir.get_type(add), Some(&RueType::I32));
    }

    #[test]
    fn test_builder_scopes() {
        let mut builder = HirBuilder::new();

        // Declare variable in outer scope
        let value1 = builder.emit_literal(42, RueType::I32);
        builder.emit_let("x", value1, RueType::I32);

        // Start inner scope
        builder.start_block();

        let value2 = builder.emit_literal(99, RueType::I32);
        builder.emit_let("y", value2, RueType::I32);

        // Should be able to access both x and y
        assert!(builder.emit_load("x").is_ok());
        assert!(builder.emit_load("y").is_ok());

        // End inner scope
        builder.end_block();

        // Should be able to access x but not y
        assert!(builder.emit_load("x").is_ok());
        assert!(builder.emit_load("y").is_err());

        let _hir = builder.finish();
    }

    #[test]
    fn test_builder_function_creation() {
        let mut builder = HirBuilder::new();

        // Create function body
        builder.start_block();
        let ret_val = builder.emit_literal(42, RueType::I32);
        builder.emit_return(Some(ret_val));
        let body = builder.end_block();

        // Create function
        let _func = builder
            .create_function("test_func", vec![], RueType::I32, body)
            .unwrap();

        let hir = builder.finish();

        assert_eq!(hir.functions.len(), 1);
        assert_eq!(hir.strings.get(hir.functions[0].name), "test_func");
    }

    #[test]
    fn test_builder_function_call() {
        let mut builder = HirBuilder::new();

        // Create arguments
        let arg1 = builder.emit_literal(10, RueType::I64);
        let arg2 = builder.emit_literal(20, RueType::I64);

        // Create function call with arguments
        let call = builder.emit_call("test_func", vec![arg1, arg2], RueType::I64);

        let hir = builder.finish();

        // Verify the call instruction was created
        assert_eq!(hir.instructions.tag(call), InstTag::Call);

        // Verify the function name is stored correctly
        let data = hir.instructions.data(call);
        let func_name_idx = StringIndex::from_raw(data.lhs);
        assert_eq!(hir.strings.get(func_name_idx), "test_func");

        // Verify arguments are stored in extra_data with count first
        let args_start = data.rhs as usize;
        assert!(args_start < hir.extra_data.len());

        // First element should be argument count
        assert_eq!(hir.extra_data[args_start], 2);

        // Next elements should be the argument instruction indices
        assert_eq!(hir.extra_data[args_start + 1], arg1.raw());
        assert_eq!(hir.extra_data[args_start + 2], arg2.raw());
    }
}
