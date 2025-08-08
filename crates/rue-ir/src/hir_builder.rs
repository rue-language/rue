//! HIR Builder - Provides a convenient API for constructing hierarchical HIR
//!
//! The builder pattern allows for incremental construction of HIR while maintaining
//! invariants and providing a clean API for the type checker.

use crate::hir::*;
use crate::types::{BindingId, RueType, StructId};
use rue_lexer::Span;
use std::collections::HashMap;

/// Scope information for variable tracking
#[derive(Debug, Clone)]
pub struct Scope {
    /// Map from variable name to binding information
    /// When a variable is shadowed, the new binding replaces the old one in this map
    variables: HashMap<String, (BindingId, ValueRef, RueType)>,
    /// Reverse map from BindingId to variable name for debugging/error messages
    binding_names: HashMap<BindingId, String>,
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

impl Scope {
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            binding_names: HashMap::new(),
        }
    }

    pub fn declare_var(
        &mut self,
        name: String,
        binding_id: BindingId,
        value_ref: ValueRef,
        ty: RueType,
    ) {
        // Store the binding information
        self.variables
            .insert(name.clone(), (binding_id, value_ref, ty));
        self.binding_names.insert(binding_id, name);
    }

    fn lookup_var(&self, name: &str) -> Option<(BindingId, ValueRef, RueType)> {
        self.variables.get(name).cloned()
    }

    /// Update the value reference for a variable by name (used for assignments)
    fn update_value_by_name(&mut self, name: &str, new_value: ValueRef) -> bool {
        if let Some((binding_id, _, ty)) = self.variables.get(name).cloned() {
            self.variables
                .insert(name.to_string(), (binding_id, new_value, ty));
            true
        } else {
            false
        }
    }

    /// Get the name associated with a binding ID (for debugging)
    pub fn get_binding_name(&self, binding_id: BindingId) -> Option<&str> {
        self.binding_names.get(&binding_id).map(|s| s.as_str())
    }

    /// Create an iterator over all variables in the scope
    /// Returns (name, binding_id, value_ref, type) for each variable
    pub fn iter(&self) -> impl Iterator<Item = (&String, BindingId, ValueRef, &RueType)> + '_ {
        self.variables
            .iter()
            .map(|(name, (binding_id, value_ref, ty))| (name, *binding_id, *value_ref, ty))
    }

    /// Check if the scope is empty
    pub fn is_empty(&self) -> bool {
        self.variables.is_empty()
    }
}

/// Builder context for a block being constructed
pub struct BlockBuilder {
    pub block_id: BlockId,
    scope: Scope,
    /// Track if this block has been explicitly terminated (e.g., by a return statement)
    has_explicit_terminator: bool,
}

impl BlockBuilder {
    /// Create a new BlockBuilder with optional predetermined BindingIds for params
    fn new_with_binding_order(
        id: BlockId,
        params: Vec<BlockParam>,
        strings: &StringPool,
        next_binding_id: &mut u32,
        binding_order: Option<&[BindingId]>,
    ) -> Self {
        let mut scope = Scope::new();

        // Add block parameters to scope using their real names
        for (i, param) in params.iter().enumerate() {
            let param_value_ref =
                ValueRef::from_block_param_with_id(id, BlockParamIndex::new(i as u32));
            let param_name = strings.get(param.name);

            // Use predetermined BindingId if available, otherwise allocate new one
            let binding_id = if let Some(order) = binding_order {
                if i < order.len() {
                    order[i]
                } else {
                    // Fallback: allocate new if order is too short
                    let id = BindingId::new(*next_binding_id);
                    *next_binding_id += 1;
                    id
                }
            } else {
                // Normal case: allocate new BindingId
                let id = BindingId::new(*next_binding_id);
                *next_binding_id += 1;
                id
            };

            scope.declare_var(
                param_name.to_string(),
                binding_id,
                param_value_ref,
                param.ty.clone(),
            );
        }

        Self {
            block_id: id,
            scope,
            has_explicit_terminator: false,
        }
    }
}

/// Builder for constructing HIR
pub struct HirBuilder {
    hir: Hir,

    /// Stack of block builders for nested block construction
    block_stack: Vec<BlockBuilder>,

    /// Global scope for tracking top-level declarations
    global_scope: Scope,

    current_span: Span,

    /// Counter for allocating unique BindingIds
    next_binding_id: u32,

    /// Map from BlockId to ordered list of BindingIds for that block's parameters
    /// Used to preserve BindingId identity across loop blocks
    block_binding_orders: std::collections::HashMap<BlockId, Vec<BindingId>>,
}

impl HirBuilder {
    /// Create a new HIR builder
    pub fn new() -> Self {
        Self {
            hir: Hir::new(),
            block_stack: Vec::new(),
            global_scope: Scope::new(),
            current_span: Span { start: 0, end: 0 },
            next_binding_id: 0,
            block_binding_orders: std::collections::HashMap::new(),
        }
    }

    /// Allocate a new unique BindingId
    pub fn alloc_binding_id(&mut self) -> BindingId {
        let id = BindingId::new(self.next_binding_id);
        self.next_binding_id += 1;
        id
    }

    /// Set the binding order for a block's parameters
    /// This tells the builder which BindingId corresponds to each parameter index
    pub fn set_block_binding_order(&mut self, block_id: BlockId, order: Vec<BindingId>) {
        self.block_binding_orders.insert(block_id, order);
    }

    /// Get the binding order for a block, if it has one
    pub fn get_block_binding_order(&self, block_id: BlockId) -> Option<&Vec<BindingId>> {
        self.block_binding_orders.get(&block_id)
    }

    /// Set the current source span for error reporting
    pub fn set_span(&mut self, span: Span) {
        self.current_span = span;
    }

    /// Start building a new block
    pub fn start_block(&mut self) -> BlockId {
        self.start_block_with_params(vec![])
    }

    /// Start building a new block with parameters
    pub fn start_block_with_params(&mut self, params: Vec<BlockParam>) -> BlockId {
        // Allocate block in the arena
        let block_id = self.hir.alloc_block(params.clone(), self.current_span);
        let binding_order = self.get_block_binding_order(block_id).cloned();
        let builder = BlockBuilder::new_with_binding_order(
            block_id,
            params,
            &self.hir.strings,
            &mut self.next_binding_id,
            binding_order.as_deref(),
        );
        self.block_stack.push(builder);
        block_id
    }

    /// End the current block and return its ID
    pub fn end_block(&mut self) -> BlockId {
        self.block_stack.pop().expect("No block to end").block_id
    }

    /// Get the current block builder (immutable)
    pub fn current_block_builder(&self) -> &BlockBuilder {
        self.block_stack.last().expect("No active block")
    }

    /// Get the current block builder (mutable)
    pub fn current_block_builder_mut(&mut self) -> &mut BlockBuilder {
        self.block_stack.last_mut().expect("No active block")
    }

    /// Get the current block ID (where we're currently inserting)
    pub fn get_current_block_id(&self) -> Option<BlockId> {
        self.block_stack.last().map(|builder| builder.block_id)
    }

    /// Get the current block mutably through the arena
    fn current_block_mut(&mut self) -> &mut Block {
        let block_id = self.current_block_builder().block_id;
        self.hir
            .block_mut(block_id)
            .expect("Block should exist in arena")
    }

    /// Get the current scope mutably
    pub fn current_scope_mut(&mut self) -> &mut Scope {
        if let Some(builder) = self.block_stack.last_mut() {
            &mut builder.scope
        } else {
            &mut self.global_scope
        }
    }

    /// SSA validation: assert that a ValueRef is live in the current block
    /// This enforces SSA form by ensuring operands are either:
    /// - Instructions from the current block
    /// - Block parameters from the current block
    fn assert_live_in_current_block(&self, value_ref: ValueRef, context: &str) {
        // Skip SSA checks if disabled (for semantic analyzer compatibility)

        let current_block_id = match self.block_stack.last() {
            Some(builder) => builder.block_id,
            None => return, // No current block, skip validation for global scope
        };

        match value_ref {
            ValueRef::Instruction { block_id, .. } => {
                if block_id != current_block_id {
                    panic!(
                        "SSA violation in {}: instruction from block {:?} used in block {:?}. \
                         All cross-block data flow must use block parameters.",
                        context, block_id, current_block_id
                    );
                }
            }
            ValueRef::BlockParam { block_id, .. } => {
                if block_id != current_block_id {
                    panic!(
                        "SSA violation in {}: block parameter from block {:?} used in block {:?}. \
                         Block parameters can only be used within their own block.",
                        context, block_id, current_block_id
                    );
                }
            }
        }
    }

    /// Validate that multiple ValueRefs are live in current block
    fn assert_values_live_in_current_block(&self, values: &[ValueRef], context: &str) {
        for (i, &value) in values.iter().enumerate() {
            self.assert_live_in_current_block(value, &format!("{} (operand {})", context, i));
        }
    }

    /// Validate that edge arguments match target block parameter types and arity
    /// This prevents type errors and arity mismatches at HIR build time
    fn validate_edge(
        &self,
        target_block_id: BlockId,
        args: &[ValueRef],
        context: &str,
    ) -> Result<(), String> {
        let target_block = self
            .hir
            .block(target_block_id)
            .ok_or_else(|| format!("Target block {:?} not found in arena", target_block_id))?;

        // Check arity
        if args.len() != target_block.params.len() {
            return Err(format!(
                "{}: argument count mismatch. Expected {} arguments for block {:?}, got {}",
                context,
                target_block.params.len(),
                target_block_id,
                args.len()
            ));
        }

        // Check types
        for (i, (&arg, param)) in args.iter().zip(target_block.params.iter()).enumerate() {
            let arg_type = self
                .get_value_type(arg)
                .ok_or_else(|| format!("{}: cannot determine type of argument {}", context, i))?;

            if arg_type != param.ty {
                return Err(format!(
                    "{}: type mismatch for argument {}. Expected {:?}, got {:?}",
                    context, i, param.ty, arg_type
                ));
            }
        }

        Ok(())
    }

    /// Emit a literal instruction
    pub fn emit_literal(&mut self, value: u64, ty: RueType) -> ValueRef {
        let inst = Instruction {
            tag: InstTag::Literal,
            data: InstData::Literal(value),
            ty: Some(ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Get variable reference (returns the ValueRef that defined it)
    /// The ValueRef represents the variable's current value
    pub fn emit_load(&mut self, name: &str) -> Result<ValueRef, String> {
        // Look up variable in current scope chain
        let var_info = self
            .lookup_variable(name)
            .ok_or_else(|| format!("Undefined variable: {}", name))?;

        // Return the ValueRef - this represents the variable
        // The ValueRef represents the binding to the variable's value
        // var_info is now (BindingId, ValueRef, RueType), so we need index 1
        Ok(var_info.1)
    }

    /// Look up a variable in the scope chain
    pub fn lookup_variable(&self, name: &str) -> Option<(BindingId, ValueRef, RueType)> {
        // Search from innermost to outermost scope
        for builder in self.block_stack.iter().rev() {
            if let Some(var) = builder.scope.lookup_var(name) {
                return Some(var);
            }
        }

        // Finally check global scope
        self.global_scope.lookup_var(name)
    }

    /// Update a variable's value in the scope chain (used for assignments)
    fn update_var_value_in_any_scope(&mut self, name: &str, new_value: ValueRef) {
        // Search from innermost to outermost scope
        for builder in self.block_stack.iter_mut().rev() {
            if builder.scope.update_value_by_name(name, new_value) {
                return;
            }
        }

        // Finally check global scope
        self.global_scope.update_value_by_name(name, new_value);
    }

    /// Emit a binary operation instruction
    pub fn emit_binary(
        &mut self,
        lhs: ValueRef,
        rhs: ValueRef,
        op: BinOp,
        result_ty: RueType,
    ) -> ValueRef {
        // SSA validation: ensure operands are live in current block
        self.assert_live_in_current_block(lhs, "emit_binary lhs");
        self.assert_live_in_current_block(rhs, "emit_binary rhs");
        let inst = Instruction {
            tag: InstTag::Binary,
            data: InstData::Binary { op, lhs, rhs },
            ty: Some(result_ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Emit a unary operation instruction
    pub fn emit_unary(&mut self, operand: ValueRef, op: UnaryOp, result_ty: RueType) -> ValueRef {
        // SSA validation: ensure operand is live in current block
        self.assert_live_in_current_block(operand, "emit_unary operand");
        let inst = Instruction {
            tag: InstTag::Unary,
            data: InstData::Unary { op, operand },
            ty: Some(result_ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Emit a function call instruction
    pub fn emit_call(
        &mut self,
        func_name: &str,
        args: Vec<ValueRef>,
        return_ty: RueType,
    ) -> ValueRef {
        // SSA validation: ensure all arguments are live in current block
        self.assert_values_live_in_current_block(&args, "emit_call arguments");

        let func_name_idx = self.hir.strings.intern(func_name);

        let inst = Instruction {
            tag: InstTag::Call,
            data: InstData::Call {
                func: func_name_idx,
                callee: None, // Will be resolved later in a resolution pass
                args,
            },
            ty: Some(return_ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Build an array literal instruction
    pub fn build_array_literal(&mut self, elements: Vec<ValueRef>, array_ty: RueType) -> ValueRef {
        // SSA validation: ensure all elements are live in current block
        self.assert_values_live_in_current_block(&elements, "build_array_literal elements");
        let inst = Instruction {
            tag: InstTag::ArrayLit,
            data: InstData::ArrayLit(elements),
            ty: Some(array_ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Build a tuple literal instruction
    pub fn build_tuple_literal(&mut self, elements: Vec<ValueRef>, tuple_ty: RueType) -> ValueRef {
        // SSA validation: ensure all elements are live in current block
        self.assert_values_live_in_current_block(&elements, "build_tuple_literal elements");
        let inst = Instruction {
            tag: InstTag::TupleLit,
            data: InstData::TupleLit(elements),
            ty: Some(tuple_ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Build a struct literal instruction
    pub fn build_struct_literal(
        &mut self,
        struct_id: StructId,
        field_values: Vec<ValueRef>,
        struct_ty: RueType,
    ) -> ValueRef {
        // SSA validation: ensure all field values are live in current block
        self.assert_values_live_in_current_block(
            &field_values,
            "build_struct_literal field_values",
        );
        let inst = Instruction {
            tag: InstTag::StructLit,
            data: InstData::StructLit {
                struct_id: struct_id.0,
                fields: field_values,
            },
            ty: Some(struct_ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Emit an array index access instruction
    pub fn emit_index_access(
        &mut self,
        base: ValueRef,
        index: ValueRef,
        element_ty: RueType,
    ) -> ValueRef {
        // SSA validation: ensure operands are live in current block
        self.assert_live_in_current_block(base, "emit_index_access base");
        self.assert_live_in_current_block(index, "emit_index_access index");
        let inst = Instruction {
            tag: InstTag::IndexAccess,
            data: InstData::IndexAccess { base, index },
            ty: Some(element_ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Emit a field access instruction (for tuples and structs)
    pub fn emit_field_access(
        &mut self,
        base: ValueRef,
        field_index: u32,
        field_ty: RueType,
    ) -> ValueRef {
        // SSA validation: ensure base is live in current block
        self.assert_live_in_current_block(base, "emit_field_access base");
        let inst = Instruction {
            tag: InstTag::FieldAccess,
            data: InstData::FieldAccess {
                base,
                field: field_index,
            },
            ty: Some(field_ty),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        ValueRef::from_inst(self.current_block_builder().block_id, inst_idx)
    }

    /// Emit a let statement (variable declaration)
    pub fn emit_let(&mut self, name: &str, init: ValueRef, var_ty: RueType) -> ValueRef {
        // SSA validation: ensure initializer is live in current block
        self.assert_live_in_current_block(init, "emit_let initializer");

        let name_idx = self.hir.strings.intern(name);

        let inst = Instruction {
            tag: InstTag::Let,
            data: InstData::Let {
                name: name_idx,
                init: Some(init),
            },
            ty: Some(var_ty.clone()),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        let value_ref = ValueRef::from_inst(self.current_block_builder().block_id, inst_idx);

        // Allocate a new BindingId for this variable declaration
        let binding_id = self.alloc_binding_id();

        // Add variable to current scope with its unique BindingId
        self.current_scope_mut()
            .declare_var(name.to_string(), binding_id, value_ref, var_ty);

        value_ref
    }

    /// Emit an assign statement
    pub fn emit_assign(&mut self, name: &str, value: ValueRef) -> Result<ValueRef, String> {
        // SSA validation: ensure assigned value is live in current block
        self.assert_live_in_current_block(value, "emit_assign value");

        // Verify variable exists in scope and get its type
        let var_info = self
            .lookup_variable(name)
            .ok_or_else(|| format!("Cannot assign to undefined variable: {}", name))?;

        let name_idx = self.hir.strings.intern(name);
        // var_info is now (BindingId, ValueRef, RueType), so type is at index 2
        let var_ty = var_info.2.clone();

        let inst = Instruction {
            tag: InstTag::Assign,
            data: InstData::Assign {
                name: name_idx,
                value,
            },
            ty: Some(var_ty.clone()),
            span: self.current_span,
        };

        let inst_idx = self.current_block_mut().push_instruction(inst);
        let assign_value_ref = ValueRef::from_inst(self.current_block_builder().block_id, inst_idx);

        // Update the value environment with the new value (the assignment result)
        // The assignment instruction produces the assigned value, so we update the environment
        // to point to the assignment instruction's result
        self.update_var_value_in_any_scope(name, assign_value_ref);

        Ok(assign_value_ref)
    }

    /// Check if the current block has been explicitly terminated
    pub fn current_block_has_terminator(&self) -> bool {
        self.current_block_builder().has_explicit_terminator
    }

    /// Set the current block's terminator to return
    pub fn set_return_terminator(&mut self, value: Option<ValueRef>) {
        // SSA validation: ensure return value is live in current block
        if let Some(val) = value {
            self.assert_live_in_current_block(val, "set_return_terminator value");
        }
        let terminator = Terminator::Return { value };
        self.current_block_mut().set_terminator(terminator);
        // Mark that this block has been explicitly terminated
        self.current_block_builder_mut().has_explicit_terminator = true;
    }

    /// Set current block's terminator to conditionally branch to two blocks
    /// Arguments must be provided explicitly - no automatic environment capture
    pub fn set_if_terminator(
        &mut self,
        cond: ValueRef,
        then_target: BlockId,
        then_args: Vec<ValueRef>,
        else_target: BlockId,
        else_args: Vec<ValueRef>,
    ) {
        // SSA validation: ensure condition and arguments are live in current block
        self.assert_live_in_current_block(cond, "set_if_terminator condition");
        self.assert_values_live_in_current_block(&then_args, "set_if_terminator then_args");
        self.assert_values_live_in_current_block(&else_args, "set_if_terminator else_args");

        // Edge validation: ensure arguments match target block parameters
        if let Err(err) =
            self.validate_edge(then_target, &then_args, "set_if_terminator then_target")
        {
            panic!("{}", err);
        }
        if let Err(err) =
            self.validate_edge(else_target, &else_args, "set_if_terminator else_target")
        {
            panic!("{}", err);
        }

        let terminator = Terminator::If {
            cond,
            then_target,
            then_args,
            else_target,
            else_args,
        };
        self.current_block_mut().set_terminator(terminator);
        self.current_block_builder_mut().has_explicit_terminator = true;
    }

    /// Set current block's terminator to jump to a target block
    /// Arguments must be provided explicitly - no automatic environment capture
    pub fn set_goto_terminator(&mut self, target: BlockId, args: Vec<ValueRef>) {
        // SSA validation: ensure arguments are live in current block
        self.assert_values_live_in_current_block(&args, "set_goto_terminator args");

        // Edge validation: ensure arguments match target block parameters
        if let Err(err) = self.validate_edge(target, &args, "set_goto_terminator") {
            panic!("{}", err);
        }
        let terminator = Terminator::Goto { target, args };
        self.current_block_mut().set_terminator(terminator);
        self.current_block_builder_mut().has_explicit_terminator = true;
    }

    /// Create a simple while loop structure using explicit blocks and terminators
    /// Returns (header_block_id, body_block_id, exit_block_id)
    pub fn create_while_loop_structure(
        &mut self,
        _loop_params: Vec<BlockParam>,
    ) -> (BlockId, BlockId, BlockId) {
        let header_id = self.hir.next_block_id();
        let body_id = self.hir.next_block_id();
        let exit_id = self.hir.next_block_id();

        (header_id, body_id, exit_id)
    }

    /// Add a break terminator to the current block
    pub fn set_break_terminator(&mut self, loop_header: BlockId, args: Vec<ValueRef>) {
        // SSA validation: ensure arguments are live in current block
        self.assert_values_live_in_current_block(&args, "set_break_terminator args");
        let terminator = Terminator::Break { loop_header, args };
        self.current_block_mut().set_terminator(terminator);
        self.current_block_builder_mut().has_explicit_terminator = true;
    }

    /// Add a continue terminator to the current block
    pub fn set_continue_terminator(&mut self, loop_header: BlockId, args: Vec<ValueRef>) {
        // SSA validation: ensure arguments are live in current block
        self.assert_values_live_in_current_block(&args, "set_continue_terminator args");
        let terminator = Terminator::Continue { loop_header, args };
        self.current_block_mut().set_terminator(terminator);
        self.current_block_builder_mut().has_explicit_terminator = true;
    }

    /// Create a function
    pub fn create_function(
        &mut self,
        name: &str,
        params: Vec<(String, RueType)>,
        return_type: RueType,
        body_id: BlockId,
    ) -> Result<FunctionId, String> {
        let name_idx = self.hir.strings.intern(name);

        // Convert params to BlockParam format
        let block_params: Vec<BlockParam> = params
            .into_iter()
            .map(|(param_name, param_ty)| BlockParam {
                name: self.hir.strings.intern(&param_name),
                ty: param_ty,
            })
            .collect();

        // Get the body block from the arena
        let body_block = self
            .hir
            .block(body_id)
            .ok_or_else(|| format!("Body block with ID {:?} not found in arena", body_id))?
            .clone();

        // Ensure function body block parameters match function parameters
        if body_block.params.len() != block_params.len() {
            return Err(format!(
                "Function body block parameter count mismatch. Function expects {} parameters, body block has {}",
                block_params.len(),
                body_block.params.len()
            ));
        }

        for (i, (func_param, body_param)) in block_params
            .iter()
            .zip(body_block.params.iter())
            .enumerate()
        {
            if func_param.name != body_param.name {
                return Err(format!(
                    "Function parameter name mismatch at position {}. Function param: '{}', body param: '{}'",
                    i,
                    self.hir.strings.get(func_param.name),
                    self.hir.strings.get(body_param.name)
                ));
            }
            if func_param.ty != body_param.ty {
                return Err(format!(
                    "Function parameter type mismatch at position {}. Function param: {:?}, body param: {:?}",
                    i, func_param.ty, body_param.ty
                ));
            }
        }

        let func = Function {
            name: name_idx,
            params: block_params,
            return_type: Some(return_type),
            body: body_block,
            span: self.current_span,
        };

        Ok(self.hir.add_function(func))
    }

    /// Get the type of an instruction in the current block
    pub fn get_instruction_type(&self, inst: LocalInstIndex) -> Option<RueType> {
        // The instruction might be in any block on the stack (from current to outer scopes)
        // Search from innermost to outermost block
        for builder_state in self.block_stack.iter().rev() {
            if let Some(block) = self.hir.block(builder_state.block_id)
                && let Some(instruction) = block.get_instruction(inst)
            {
                return instruction.ty.clone();
            }
        }
        None
    }

    /// Get the current variable type by name
    pub fn get_variable_type(&self, name: &str) -> Option<RueType> {
        // var_info is now (BindingId, ValueRef, RueType), so type is at index 2
        self.lookup_variable(name).map(|(_, _, ty)| ty)
    }

    /// Get the type of a value reference (works with both instructions and block parameters)
    pub fn get_value_type(&self, value_ref: ValueRef) -> Option<RueType> {
        match value_ref {
            ValueRef::Instruction { block_id, index } => {
                // Get the instruction type from the specified block
                if let Some(block) = self.hir.block(block_id)
                    && let Some(instruction) = block.get_instruction(index)
                {
                    return instruction.ty.clone();
                }
                None
            }
            ValueRef::BlockParam { block_id, index } => {
                // Get the block parameter type from the specified block
                if let Some(block) = self.hir.block(block_id)
                    && let Some(param) = block.params.get(index.as_usize())
                {
                    return Some(param.ty.clone());
                }
                None
            }
        }
    }

    /// Create a ValueRef from a LocalInstIndex (convenience method)
    pub fn value_from_inst(&self, inst: LocalInstIndex) -> ValueRef {
        ValueRef::from_instruction(inst)
    }

    /// Create a ValueRef from a block parameter index (convenience method)
    pub fn value_from_param(&self, param_idx: u32) -> ValueRef {
        ValueRef::from_param_index(param_idx)
    }

    /// Intern a string in the HIR string pool
    pub fn intern_string(&mut self, s: &str) -> StringIndex {
        self.hir.strings.intern(s)
    }

    /// Add a function to the HIR
    pub fn add_function(&mut self, func: Function) -> FunctionId {
        self.hir.add_function(func)
    }

    /// Switch to building instructions in a specific block
    /// This is used for if-expressions where we need to continue building in the merge block
    pub fn switch_to_block(&mut self, block_id: BlockId) {
        // Get the block's parameters for the scope
        let block = self.hir.block(block_id).expect("Block should exist");
        let params = block.params.clone();

        // Create a new BlockBuilder for this block and push it onto the stack
        let binding_order = self.get_block_binding_order(block_id).cloned();
        let builder = BlockBuilder::new_with_binding_order(
            block_id,
            params,
            &self.hir.strings,
            &mut self.next_binding_id,
            binding_order.as_deref(),
        );
        self.block_stack.push(builder);
    }

    /// Set the insertion point to a specific block without pushing onto the stack
    /// This changes the current block where new instructions will be added.
    /// Block must already exist with the correct parameters.
    /// No automatic environment capture - caller must handle parameter threading explicitly.
    pub fn set_insertion_point(&mut self, block_id: BlockId) {
        // Get the target block's existing parameters
        let block = self.hir.block(block_id).expect("Block should exist");
        let existing_params = block.params.clone();

        // Create builder with existing parameters, preserving binding order if available
        let binding_order = self.get_block_binding_order(block_id).cloned();
        if let Some(current_builder) = self.block_stack.last_mut() {
            *current_builder = BlockBuilder::new_with_binding_order(
                block_id,
                existing_params,
                &self.hir.strings,
                &mut self.next_binding_id,
                binding_order.as_deref(),
            );
        } else {
            let builder = BlockBuilder::new_with_binding_order(
                block_id,
                existing_params,
                &self.hir.strings,
                &mut self.next_binding_id,
                binding_order.as_deref(),
            );
            self.block_stack.push(builder);
        }
    }

    /// Validate that all blocks have been properly closed
    pub fn validate_block_nesting(&self) -> Result<(), String> {
        if !self.block_stack.is_empty() {
            Err(format!(
                "Unmatched start_block() calls: {} blocks left open",
                self.block_stack.len()
            ))
        } else {
            Ok(())
        }
    }

    /// Build an if-expression with proper SSA form using block parameters
    /// Returns (merge_block_id, phi_value_ref) where phi_value_ref represents the result
    pub fn build_if_expr(
        &mut self,
        condition: ValueRef,
        then_builder: impl FnOnce(&mut Self) -> ValueRef,
        else_builder: impl FnOnce(&mut Self) -> ValueRef,
        result_ty: RueType,
    ) -> (BlockId, ValueRef) {
        // SSA validation: ensure condition is live in current block
        self.assert_live_in_current_block(condition, "build_if_expr condition");

        // Allocate block IDs first
        let then_block_id = self.hir.alloc_block(vec![], self.current_span);
        let else_block_id = self.hir.alloc_block(vec![], self.current_span);

        // Create merge block with a parameter to receive the result
        let merge_param = BlockParam {
            name: self.hir.strings.intern("__if_result"),
            ty: result_ty.clone(),
        };
        let merge_block_id = self.start_block_with_params(vec![merge_param]);
        let phi_value = ValueRef::from_block_param_with_id(merge_block_id, BlockParamIndex::new(0));
        self.end_block(); // End merge block construction

        // Set conditional terminator in the current block before building branches
        self.set_if_terminator(condition, then_block_id, vec![], else_block_id, vec![]);

        // Build then block
        self.switch_to_block(then_block_id);
        let then_value = then_builder(self);
        self.set_goto_terminator(merge_block_id, vec![then_value]);
        self.end_block();

        // Build else block
        self.switch_to_block(else_block_id);
        let else_value = else_builder(self);
        self.set_goto_terminator(merge_block_id, vec![else_value]);
        self.end_block();

        // Switch to merge block for continued building
        self.set_insertion_point(merge_block_id);

        (merge_block_id, phi_value)
    }

    /// Build a while loop with proper SSA form using block parameters
    /// Returns (exit_block_id, loop_values) where loop_values are the final loop state
    pub fn build_while(
        &mut self,
        initial_values: Vec<(String, ValueRef, RueType)>, // (name, initial_value, type)
        condition_builder: impl FnOnce(&mut Self, &[ValueRef]) -> ValueRef,
        body_builder: impl FnOnce(&mut Self, &[ValueRef]) -> Vec<ValueRef>,
    ) -> (BlockId, Vec<ValueRef>) {
        // SSA validation: ensure initial values are live in current block
        for (name, value, _) in &initial_values {
            self.assert_live_in_current_block(
                *value,
                &format!("build_while initial value '{}'", name),
            );
        }

        // Create loop header block with parameters for loop state
        let header_params: Vec<BlockParam> = initial_values
            .iter()
            .map(|(name, _, ty)| BlockParam {
                name: self.hir.strings.intern(name),
                ty: ty.clone(),
            })
            .collect();

        let header_block_id = self.hir.alloc_block(header_params, self.current_span);
        let header_values: Vec<ValueRef> = (0..initial_values.len())
            .map(|i| {
                ValueRef::from_block_param_with_id(header_block_id, BlockParamIndex::new(i as u32))
            })
            .collect();

        // Allocate body and exit blocks
        let body_block_id = self.hir.alloc_block(vec![], self.current_span);

        let exit_params: Vec<BlockParam> = initial_values
            .iter()
            .map(|(name, _, ty)| BlockParam {
                name: self.hir.strings.intern(&format!("{}_final", name)),
                ty: ty.clone(),
            })
            .collect();
        let exit_block_id = self.hir.alloc_block(exit_params, self.current_span);
        let exit_values: Vec<ValueRef> = (0..initial_values.len())
            .map(|i| {
                ValueRef::from_block_param_with_id(exit_block_id, BlockParamIndex::new(i as u32))
            })
            .collect();

        // Jump to header from current block first
        let initial_arg_values: Vec<ValueRef> =
            initial_values.iter().map(|(_, val, _)| *val).collect();
        self.set_goto_terminator(header_block_id, initial_arg_values);

        // Build header block
        self.switch_to_block(header_block_id);
        let condition = condition_builder(self, &header_values);
        self.set_if_terminator(
            condition,
            body_block_id,
            vec![],
            exit_block_id,
            header_values.clone(),
        );
        self.end_block();

        // Build body block
        self.switch_to_block(body_block_id);
        let updated_values = body_builder(self, &header_values);
        self.set_goto_terminator(header_block_id, updated_values);
        self.end_block();

        // Switch to exit block for continued building
        self.set_insertion_point(exit_block_id);

        (exit_block_id, exit_values)
    }

    /// Capture the current environment (all visible variables)
    /// Returns a mapping from variable names to their BindingIds, ValueRefs and types
    pub fn capture_environment(&self) -> Vec<(BindingId, String, ValueRef, RueType)> {
        let mut captured = Vec::new();
        let mut seen = HashMap::new();

        // Capture from innermost to outermost scope (innermost shadows outer)
        for builder in self.block_stack.iter().rev() {
            for (name, binding_id, value_ref, ty) in builder.scope.iter() {
                if !seen.contains_key(name) {
                    seen.insert(name.clone(), ());
                    captured.push((binding_id, name.clone(), value_ref, ty.clone()));
                }
            }
        }

        // Also capture global scope
        for (name, binding_id, value_ref, ty) in self.global_scope.iter() {
            if !seen.contains_key(name) {
                captured.push((binding_id, name.clone(), value_ref, ty.clone()));
            }
        }

        // Sort by BindingId for stable ordering
        captured.sort_by_key(|(binding_id, _, _, _)| *binding_id);

        captured
    }

    // Structured While Loop Construction API
    //
    // These methods provide a minimal scaffolding for building SSA-correct while loops.
    // The pattern follows: while_begin -> while_set_cond -> while_prepare_exit_from_header -> while_backedge

    /// Begin constructing a while loop with loop-carried variables
    ///
    /// This prepares the loop blocks and wires preheader -> header.
    ///
    /// Arguments:
    /// - carried: (binding_id, name, initial_value, type) for each loop-carried variable
    ///
    /// Returns: WhileCtx for subsequent helper calls
    ///
    /// Structure created:
    /// ```text
    /// current_block ──goto(args=initial_values)──▶ header(params=LC...)
    /// ```
    ///
    /// IMPORTANT: This method preserves BindingId identity across loop blocks
    /// to fix the "BindingId drift" issue where loop variables get new IDs.
    pub fn while_begin(
        &mut self,
        carried: Vec<(BindingId, String, ValueRef, RueType)>,
    ) -> WhileCtx {
        // SSA validation: ensure initial values are live in current block
        for (_, name, value, _) in &carried {
            self.assert_live_in_current_block(
                *value,
                &format!("while_begin initial value '{}'", name),
            );
        }

        // Extract the components we need
        let loop_carried_vars: Vec<(BindingId, String, RueType)> = carried
            .iter()
            .map(|(bid, name, _, ty)| (*bid, name.clone(), ty.clone()))
            .collect();
        let binding_order: Vec<BindingId> = carried.iter().map(|(bid, _, _, _)| *bid).collect();

        // Create blocks using alloc_block (which allocates IDs automatically)
        let header_params: Vec<BlockParam> = loop_carried_vars
            .iter()
            .map(|(_, name, ty)| BlockParam {
                name: self.hir.strings.intern(name),
                ty: ty.clone(),
            })
            .collect();

        let header_id = self.hir.alloc_block(header_params, self.current_span);
        // Body block gets the same param list as header (explicit, no auto-capture)
        let body_params: Vec<BlockParam> = loop_carried_vars
            .iter()
            .map(|(_, name, ty)| BlockParam {
                name: self.hir.strings.intern(name),
                ty: ty.clone(),
            })
            .collect();
        let body_id = self.hir.alloc_block(body_params, self.current_span);
        // Create exit block with same parameters as header/body for final values
        let exit_params: Vec<BlockParam> = loop_carried_vars
            .iter()
            .map(|(_, name, ty)| BlockParam {
                name: self.hir.strings.intern(name),
                ty: ty.clone(),
            })
            .collect();
        let exit_id = self.hir.alloc_block(exit_params, self.current_span);

        // Set binding orders for loop blocks to preserve BindingId identity
        // Always set binding order for loop blocks, even if empty, to mark them as loop blocks
        self.set_block_binding_order(header_id, binding_order.clone());
        self.set_block_binding_order(body_id, binding_order.clone());
        self.set_block_binding_order(exit_id, binding_order.clone());

        // Guardrails: assert block arity equals carried.len()
        debug_assert_eq!(
            self.hir.block(header_id).unwrap().params.len(),
            binding_order.len(),
            "header arity mismatch"
        );
        debug_assert_eq!(
            self.hir.block(body_id).unwrap().params.len(),
            binding_order.len(),
            "body arity mismatch"
        );
        debug_assert_eq!(
            self.hir.block(exit_id).unwrap().params.len(),
            binding_order.len(),
            "exit arity mismatch"
        );

        // Create context
        let ctx = WhileCtx::new(header_id, body_id, exit_id, loop_carried_vars);

        // Wire preheader -> header with initial values
        let initial_arg_values: Vec<ValueRef> = carried.iter().map(|(_, _, val, _)| *val).collect();
        self.set_goto_terminator(header_id, initial_arg_values);

        ctx
    }

    /// Set the loop condition and branch from header block
    ///
    /// This switches to the header block, evaluates the condition, and sets up the conditional branch.
    ///
    /// Arguments:
    /// - ctx: WhileCtx from while_begin
    /// - condition_builder: closure that builds the condition using header block parameters
    ///
    /// Returns: ValueRefs for header block parameters (loop-carried variables)
    ///
    /// Structure after this call:
    /// ```text
    /// header(params=LC...) ──if(cond)──▶ body ──(to be wired later)──▶ header
    ///     └────────────────────────────▶ exit (to be wired later)
    /// ```
    pub fn while_set_cond<F>(&mut self, ctx: &WhileCtx, condition_builder: F) -> Vec<ValueRef>
    where
        F: FnOnce(&mut Self, &[ValueRef]) -> ValueRef,
    {
        // Switch to header block
        self.set_insertion_point(ctx.header_block_id);

        // Get header parameter ValueRefs for loop-carried variables
        let header_values = ctx.header_value_refs();

        // Build condition using header parameters
        let condition = condition_builder(self, &header_values);

        // Set conditional branch: if(cond) then body else exit
        // Pass header values explicitly to both body and exit
        self.set_if_terminator(
            condition,
            ctx.body_block_id,
            header_values.clone(), // Pass header values to body explicitly
            ctx.exit_block_id,
            header_values.clone(), // Pass header values to exit explicitly
        );

        header_values
    }

    /// Prepare the exit block to receive loop-carried variables from header
    ///
    /// This ensures the exit block has proper parameters and patches the else edge
    /// from the header to pass the current loop state.
    ///
    /// Arguments:
    /// - ctx: WhileCtx from while_begin
    /// - header_values: ValueRefs from while_set_cond representing current loop state
    ///
    /// Structure after this call:
    /// ```text
    /// header(params=LC...) ──if(cond)──▶ body
    ///     └──────────────────else(args=LC...)──▶ exit(params=LC...)
    /// ```
    pub fn while_prepare_exit_from_header(
        &mut self,
        ctx: &mut WhileCtx,
        header_values: &[ValueRef],
    ) {
        // SSA validation: ensure header values are from header block
        for (i, &value) in header_values.iter().enumerate() {
            if value.block_id() != ctx.header_block_id {
                panic!(
                    "SSA violation in while_prepare_exit_from_header: header value {} is from block {:?}, expected {:?}",
                    i,
                    value.block_id(),
                    ctx.header_block_id
                );
            }
        }

        // Exit block already has parameters set up in while_begin
        // No need to update them here

        // Update the header block's else branch to pass header values to exit
        if let Some(header_block) = self.hir.block_mut(ctx.header_block_id)
            && let Terminator::If {
                else_target,
                else_args,
                ..
            } = &mut header_block.term
            && *else_target == ctx.exit_block_id
        {
            *else_args = header_values.to_vec();
        }

        ctx.exit_prepared = true;
    }

    /// Emit a backedge from body to header with updated loop-carried variables
    ///
    /// This should be called from within the body block after generating body instructions.
    /// It wires the body -> header backedge with updated values.
    ///
    /// Arguments:
    /// - ctx: WhileCtx from while_begin  
    /// - updated_values: New values for loop-carried variables to pass back to header
    ///
    /// Structure after this call:
    /// ```text
    /// header(params=LC...) ──if(cond)──▶ body ──goto(args=updated)──▶ header
    ///     └──────────────────else(args=LC...)──▶ exit(params=LC...)
    /// ```
    pub fn while_backedge(&mut self, ctx: &WhileCtx, updated_values: Vec<ValueRef>) {
        // SSA validation: ensure updated values are live in current block
        self.assert_values_live_in_current_block(&updated_values, "while_backedge updated_values");

        // Validate argument count matches loop-carried variables
        if updated_values.len() != ctx.loop_carried_vars.len() {
            panic!(
                "while_backedge: argument count mismatch. Expected {} arguments for loop-carried variables, got {}",
                ctx.loop_carried_vars.len(),
                updated_values.len()
            );
        }

        // Emit backedge: body -> header with updated values
        self.set_goto_terminator(ctx.header_block_id, updated_values);
    }

    /// Finish building and return the completed HIR
    pub fn finish(self) -> Result<Hir, String> {
        // Validate that all blocks were properly closed
        if !self.block_stack.is_empty() {
            return Err(format!(
                "Cannot finish HIR: {} unclosed blocks remaining",
                self.block_stack.len()
            ));
        }

        Ok(self.hir)
    }
}

/// Context for structured while loop construction
/// Tracks the loop structure and ensures proper SSA form with block parameters
#[derive(Debug, Clone)]
pub struct WhileCtx {
    /// Loop header block - receives loop-carried variables as parameters
    pub header_block_id: BlockId,
    /// Loop body block - where loop body instructions are placed
    pub body_block_id: BlockId,
    /// Loop exit block - where control flows after loop ends
    pub exit_block_id: BlockId,
    /// Loop-carried variables: (binding_id, name, type) tuples
    /// These become block parameters for header and exit blocks
    /// BindingId ensures we track the actual variable identity, not just names
    pub loop_carried_vars: Vec<(BindingId, String, RueType)>,
    /// Track if exit block has been prepared with parameters
    exit_prepared: bool,
}

impl WhileCtx {
    /// Create a new while loop context with allocated block IDs
    fn new(
        header_id: BlockId,
        body_id: BlockId,
        exit_id: BlockId,
        loop_carried_vars: Vec<(BindingId, String, RueType)>,
    ) -> Self {
        Self {
            header_block_id: header_id,
            body_block_id: body_id,
            exit_block_id: exit_id,
            loop_carried_vars,
            exit_prepared: false,
        }
    }

    /// Create ValueRefs for header block parameters
    pub fn create_header_value_refs(&self) -> Vec<ValueRef> {
        (0..self.loop_carried_vars.len())
            .map(|i| {
                ValueRef::from_block_param_with_id(
                    self.header_block_id,
                    BlockParamIndex::new(i as u32),
                )
            })
            .collect()
    }

    /// Get header block parameters for loop-carried variables  
    #[cfg(test)]
    fn header_params(&self, strings: &mut StringPool) -> Vec<BlockParam> {
        self.loop_carried_vars
            .iter()
            .map(|(_, name, ty)| {
                BlockParam {
                    name: strings.intern(name), // Use the original name
                    ty: ty.clone(),
                }
            })
            .collect()
    }

    /// Get exit block parameters for loop-carried variables  
    #[cfg(test)]
    fn exit_params(&self, strings: &mut StringPool) -> Vec<BlockParam> {
        self.loop_carried_vars
            .iter()
            .map(|(_, name, ty)| {
                BlockParam {
                    name: strings.intern(&format!("{}_final", name)), // Add "_final" suffix for exit
                    ty: ty.clone(),
                }
            })
            .collect()
    }

    /// Create ValueRefs for header block parameters
    fn header_value_refs(&self) -> Vec<ValueRef> {
        (0..self.loop_carried_vars.len())
            .map(|i| {
                ValueRef::from_block_param_with_id(
                    self.header_block_id,
                    BlockParamIndex::new(i as u32),
                )
            })
            .collect()
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

        builder.start_block();
        let _int_lit = builder.emit_literal(42, RueType::I32);
        let _bool_lit = builder.emit_literal(1, RueType::Bool);
        let block_id = builder.end_block();

        let hir = builder.finish().unwrap();
        let block = hir.block(block_id).unwrap();
        assert_eq!(block.body.len(), 2);
        assert_eq!(block.body[0].tag, InstTag::Literal);
        assert_eq!(block.body[1].tag, InstTag::Literal);
    }

    #[test]
    fn test_builder_variables() {
        let mut builder = HirBuilder::new();

        builder.start_block();

        // Create a variable
        let init_val_ref = builder.emit_literal(42, RueType::I32);
        let _let_inst = builder.emit_let("x", init_val_ref, RueType::I32);

        // Load the variable (returns the Let instruction that represents the variable)
        let var_ref = builder.emit_load("x").expect("Should find variable");
        // The let instruction itself doesn't have a type, but the variable does
        // We can verify the variable was properly registered
        assert!(builder.lookup_variable("x").is_some());
        let var_info = builder.lookup_variable("x").unwrap();
        // var_info is now (BindingId, ValueRef, RueType)
        assert_eq!(var_info.2, RueType::I32);
        // var_ref should be the let instruction that binds the variable
        assert_eq!(var_ref, var_info.1); // emit_load returns the let instruction

        let block_id = builder.end_block();
        let hir = builder.finish().unwrap();
        let block = hir.block(block_id).unwrap();
        assert_eq!(block.body.len(), 2); // literal, let (no load instruction anymore)
    }

    #[test]
    fn test_builder_conditional_terminator() {
        let mut builder = HirBuilder::new();

        // Create actual target blocks (not just IDs)
        let then_target = builder.start_block();
        builder.end_block();
        let else_target = builder.start_block();
        builder.end_block();

        // Main block
        builder.start_block();

        // Condition
        let cond_ref = builder.emit_literal(1, RueType::Bool);

        // Set conditional terminator
        builder.set_if_terminator(cond_ref, then_target, vec![], else_target, vec![]);

        let main_block_id = builder.end_block();
        let hir = builder.finish().unwrap();
        let main_block = hir.block(main_block_id).unwrap();

        // Verify structure
        assert_eq!(main_block.body.len(), 1); // only condition literal

        // Check that block has conditional terminator
        if let Terminator::If {
            cond: _,
            then_target: t,
            else_target: e,
            ..
        } = &main_block.term
        {
            assert_eq!(*t, then_target);
            assert_eq!(*e, else_target);
        } else {
            panic!("Expected If terminator");
        }
    }

    #[test]
    fn test_builder_goto_terminator() {
        let mut builder = HirBuilder::new();

        // Create target block with a parameter to accept the argument
        let param = BlockParam {
            name: builder.hir.strings.intern("val"),
            ty: RueType::I32,
        };
        let target_id = builder.start_block_with_params(vec![param]);
        builder.end_block();

        // Main block
        builder.start_block();

        // Some instruction
        let val_ref = builder.emit_literal(42, RueType::I32);

        // Set goto terminator with arguments
        builder.set_goto_terminator(target_id, vec![val_ref]);

        let main_block_id = builder.end_block();
        let hir = builder.finish().unwrap();
        let main_block = hir.block(main_block_id).unwrap();

        // Verify structure
        assert_eq!(main_block.body.len(), 1); // literal instruction

        // Check that block has goto terminator
        if let Terminator::Goto { target, args } = &main_block.term {
            assert_eq!(*target, target_id);
            assert_eq!(args.len(), 1);
            assert!(args[0].is_instruction());
        } else {
            panic!("Expected Goto terminator");
        }
    }

    #[test]
    fn test_block_nesting_validation() {
        let mut builder = HirBuilder::new();

        builder.start_block();
        // Don't end the block

        let result = builder.validate_block_nesting();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("1 blocks left open"));
    }

    #[test]
    fn test_value_ref_in_binary_operations() {
        let mut builder = HirBuilder::new();

        builder.start_block();

        // Create two literals
        let val1 = builder.emit_literal(5, RueType::I32);
        let val2 = builder.emit_literal(10, RueType::I32);

        // Use in binary operation
        let _add_inst = builder.emit_binary(val1, val2, BinOp::Add, RueType::I32);

        let block_id = builder.end_block();
        let hir = builder.finish().unwrap();
        let block = hir.block(block_id).unwrap();
        assert_eq!(block.body.len(), 3); // two literals + binary op

        // Verify the binary operation uses ValueRef correctly
        if let InstData::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
        } = &block.body[2].data
        {
            assert!(lhs.is_instruction());
            assert!(rhs.is_instruction());
        } else {
            panic!("Expected Binary instruction");
        }
    }

    #[test]
    fn test_value_ref_in_function_calls() {
        let mut builder = HirBuilder::new();

        builder.start_block();

        // Create arguments
        let arg1 = builder.emit_literal(42, RueType::I32);
        let arg2 = builder.emit_literal(24, RueType::I32);

        // Use in function call
        let args = vec![arg1, arg2];
        let _call_inst = builder.emit_call("test_func", args, RueType::I32);

        let block_id = builder.end_block();
        let hir = builder.finish().unwrap();
        let block = hir.block(block_id).unwrap();
        assert_eq!(block.body.len(), 3); // two literals + call

        // Verify the call uses ValueRef correctly
        if let InstData::Call {
            func: _,
            callee: _,
            args,
        } = &block.body[2].data
        {
            assert_eq!(args.len(), 2);
            assert!(args[0].is_instruction());
            assert!(args[1].is_instruction());
        } else {
            panic!("Expected Call instruction");
        }
    }

    #[test]
    fn test_block_param_value_refs() {
        // Test creating ValueRef from block parameters
        let mut builder = HirBuilder::new();

        // Create block with parameters
        let param1 = BlockParam {
            name: builder.hir.strings.intern("param1"),
            ty: RueType::I32,
        };
        let param2 = BlockParam {
            name: builder.hir.strings.intern("param2"),
            ty: RueType::Bool,
        };

        let block_id = builder.start_block_with_params(vec![param1, param2]);

        // After starting a block with params, we can access them by name
        let param_val1 = builder.emit_load("param1").unwrap();
        let param_val2 = builder.emit_load("param2").unwrap();

        assert!(param_val1.is_block_param());
        assert!(param_val2.is_block_param());
        assert_ne!(param_val1, param_val2);

        // Use block parameter in binary operation
        let _binary_inst = builder.emit_binary(param_val1, param_val2, BinOp::Eq, RueType::Bool);

        builder.end_block();
        let hir = builder.finish().unwrap();
        let block = hir.block(block_id).unwrap();
        assert_eq!(block.body.len(), 1); // only the binary op
        assert_eq!(block.params.len(), 2);

        // Verify the binary operation uses block parameter ValueRefs
        if let InstData::Binary { lhs, rhs, .. } = &block.body[0].data {
            assert!(lhs.is_block_param());
            assert!(rhs.is_block_param());
            assert_eq!(lhs.as_block_param().unwrap().raw(), 0);
            assert_eq!(rhs.as_block_param().unwrap().raw(), 1);
        } else {
            panic!("Expected Binary instruction");
        }
    }

    #[test]
    fn test_if_terminator_with_arguments() {
        let mut builder = HirBuilder::new();

        // Create target blocks with parameters to accept the arguments
        let param = BlockParam {
            name: builder.hir.strings.intern("phi"),
            ty: RueType::I32,
        };
        let then_id = builder.start_block_with_params(vec![param.clone()]);
        builder.end_block();
        let else_id = builder.start_block_with_params(vec![param]);
        builder.end_block();

        // Main block
        builder.start_block();

        // Condition and values for phi nodes
        let cond_ref = builder.emit_literal(1, RueType::Bool);
        let val1_ref = builder.emit_literal(10, RueType::I32);
        let val2_ref = builder.emit_literal(20, RueType::I32);

        // Set conditional terminator with arguments
        builder.set_if_terminator(
            cond_ref,
            then_id,
            vec![val1_ref], // then_args
            else_id,
            vec![val2_ref], // else_args
        );

        let main_block_id = builder.end_block();
        let hir = builder.finish().unwrap();
        let main_block = hir.block(main_block_id).unwrap();

        // Verify structure
        assert_eq!(main_block.body.len(), 3); // cond, val1, val2

        // Check the if terminator has block arguments
        if let Terminator::If {
            cond: _,
            then_target,
            then_args,
            else_target,
            else_args,
        } = &main_block.term
        {
            assert_eq!(*then_target, then_id);
            assert_eq!(*else_target, else_id);
            assert_eq!(then_args.len(), 1);
            assert_eq!(else_args.len(), 1);
            assert!(then_args[0].is_instruction());
            assert!(else_args[0].is_instruction());
        } else {
            panic!("Expected If terminator");
        }
    }

    #[test]
    fn test_loop_structure_with_terminators() {
        let mut builder = HirBuilder::new();

        // Create loop structure with header that has a parameter
        let param = BlockParam {
            name: builder.hir.strings.intern("i"),
            ty: RueType::I32,
        };
        let header_id = builder.start_block_with_params(vec![param]);
        builder.end_block();

        let body_id = builder.start_block();
        builder.end_block();

        let exit_id = builder.start_block();
        builder.end_block();

        // Main block - sets up initial values and jumps to header
        builder.start_block();
        let init_ref = builder.emit_literal(0, RueType::I32);

        // Jump to loop header with initial value
        builder.set_goto_terminator(header_id, vec![init_ref]);
        let main_block_id = builder.end_block();
        let hir = builder.finish().unwrap();
        let main_block = hir.block(main_block_id).unwrap();

        // Verify structure
        assert_eq!(main_block.body.len(), 1); // init value

        // Check goto terminator
        if let Terminator::Goto { target, args } = &main_block.term {
            assert_eq!(*target, header_id);
            assert_eq!(args.len(), 1);
            assert!(args[0].is_instruction());
        } else {
            panic!("Expected Goto terminator");
        }

        // Verify the structure created the right block IDs
        assert_ne!(header_id, body_id);
        assert_ne!(body_id, exit_id);
        assert_ne!(header_id, exit_id);
    }

    #[test]
    fn test_break_and_continue_terminators() {
        let mut builder = HirBuilder::new();

        // Create loop header ID
        let loop_header = builder.hir.next_block_id();

        // Test break terminator
        builder.start_block();
        let break_ref = builder.emit_literal(42, RueType::I32);
        builder.set_break_terminator(loop_header, vec![break_ref]);
        let break_block_id = builder.end_block();

        // Test continue terminator
        builder.start_block();
        let cont_ref = builder.emit_literal(100, RueType::I32);
        builder.set_continue_terminator(loop_header, vec![cont_ref]);
        let continue_block_id = builder.end_block();

        let hir = builder.finish().unwrap();
        let break_block = hir.block(break_block_id).unwrap();
        let continue_block = hir.block(continue_block_id).unwrap();

        // Verify break terminator
        if let Terminator::Break {
            loop_header: lh,
            args,
        } = &break_block.term
        {
            assert_eq!(*lh, loop_header);
            assert_eq!(args.len(), 1);
            assert!(args[0].is_instruction());
        } else {
            panic!("Expected Break terminator");
        }

        // Verify continue terminator
        if let Terminator::Continue {
            loop_header: lh,
            args,
        } = &continue_block.term
        {
            assert_eq!(*lh, loop_header);
            assert_eq!(args.len(), 1);
            assert!(args[0].is_instruction());
        } else {
            panic!("Expected Continue terminator");
        }
    }

    // SSA Safety Tests - These tests ensure the SSA invariants are properly enforced

    #[test]
    #[should_panic(expected = "SSA violation")]
    fn test_cross_block_instruction_use_detected() {
        let mut builder = HirBuilder::new();

        // Create first block with an instruction
        builder.start_block();
        let val_from_block1 = builder.emit_literal(42, RueType::I32);
        let _block1_id = builder.end_block();

        // Create second block and try to use instruction from first block
        builder.start_block();
        let val2 = builder.emit_literal(10, RueType::I32);

        // This should panic with SSA violation
        let _result = builder.emit_binary(val_from_block1, val2, BinOp::Add, RueType::I32);
    }

    #[test]
    #[should_panic(expected = "argument count mismatch")]
    fn test_edge_arity_validation() {
        let mut builder = HirBuilder::new();

        // Create target block with no parameters
        let target_id = builder.start_block();
        builder.end_block();

        // Create source block
        builder.start_block();
        let val = builder.emit_literal(42, RueType::I32);

        // Try to pass an argument to a block that expects none
        // This should panic with argument count mismatch
        builder.set_goto_terminator(target_id, vec![val]);
    }

    #[test]
    fn test_build_if_expr_generates_correct_ssa() {
        let mut builder = HirBuilder::new();

        builder.start_block();
        let cond = builder.emit_literal(1, RueType::Bool);

        let (merge_id, result) = builder.build_if_expr(
            cond,
            |builder| {
                // Simple computation in then branch
                let a = builder.emit_literal(20, RueType::I32);
                let b = builder.emit_literal(5, RueType::I32);
                builder.emit_binary(a, b, BinOp::Add, RueType::I32)
            },
            |builder| {
                // Simple computation in else branch
                let a = builder.emit_literal(30, RueType::I32);
                let b = builder.emit_literal(10, RueType::I32);
                builder.emit_binary(a, b, BinOp::Sub, RueType::I32)
            },
            RueType::I32,
        );

        // Verify we can use the result (it's a block parameter from merge block)
        let one = builder.emit_literal(1, RueType::I32);
        let _final_result = builder.emit_binary(result, one, BinOp::Add, RueType::I32);

        let _main_block = builder.end_block();
        let _hir = builder.finish().unwrap();

        // Verify the result is a block parameter from the merge block
        assert!(result.is_block_param());
        assert_eq!(result.block_id(), merge_id);
    }

    #[test]
    fn test_set_insertion_point_creates_fresh_scope() {
        let mut builder = HirBuilder::new();

        // Create a block with parameters
        let param1 = BlockParam {
            name: builder.hir.strings.intern("a"),
            ty: RueType::I32,
        };
        let param2 = BlockParam {
            name: builder.hir.strings.intern("b"),
            ty: RueType::Bool,
        };

        let target_id = builder.start_block_with_params(vec![param1, param2]);
        builder.end_block();

        // Create main block with some variables
        builder.start_block();
        let val = builder.emit_literal(42, RueType::I32);
        let _ = builder.emit_let("x", val, RueType::I32);
        let _main_block = builder.end_block();

        // Switch to target block - should adopt fresh scope from params
        builder.set_insertion_point(target_id);

        // Should be able to access block parameters by their real names
        let a_val = builder.emit_load("a");
        let b_val = builder.emit_load("b");
        assert!(a_val.is_ok());
        assert!(b_val.is_ok());

        // Should NOT be able to access variables from previous scope
        let x_val = builder.emit_load("x");
        assert!(x_val.is_err()); // x should not be in scope
    }

    #[test]
    fn test_block_param_binding_uses_real_names() {
        let mut builder = HirBuilder::new();

        // Create block with named parameters
        let param1 = BlockParam {
            name: builder.hir.strings.intern("counter"),
            ty: RueType::I32,
        };
        let param2 = BlockParam {
            name: builder.hir.strings.intern("flag"),
            ty: RueType::Bool,
        };

        let block_id = builder.start_block_with_params(vec![param1, param2]);

        // Should be able to load variables by their real names
        let counter_val = builder.emit_load("counter").unwrap();
        let flag_val = builder.emit_load("flag").unwrap();

        // Values should be block parameters
        assert!(counter_val.is_block_param());
        assert!(flag_val.is_block_param());
        assert_eq!(counter_val.block_id(), block_id);
        assert_eq!(flag_val.block_id(), block_id);

        // Should be able to use them in operations
        let one = builder.emit_literal(1, RueType::I32);
        let _result = builder.emit_binary(counter_val, one, BinOp::Add, RueType::I32);

        builder.end_block();
        let _hir = builder.finish().unwrap();
    }

    #[test]
    fn test_structured_helpers_exist() {
        // Simple test to verify the structured helpers compile
        let mut builder = HirBuilder::new();

        // Test build_if_expr exists with correct signature
        builder.start_block();
        let cond = builder.emit_literal(1, RueType::Bool);
        let (_merge_id, result) = builder.build_if_expr(
            cond,
            |_builder| _builder.emit_literal(10, RueType::I32),
            |_builder| _builder.emit_literal(20, RueType::I32),
            RueType::I32,
        );

        // Verify result is a block parameter
        assert!(result.is_block_param());

        builder.end_block();
        let _hir = builder.finish().unwrap();
    }

    #[test]
    fn test_environment_capture_in_if_branches() {
        // Test that variables declared before an if statement
        // remain accessible in both branches through environment capture
        let mut builder = HirBuilder::new();

        builder.start_block();

        // Declare variables before the if statement
        let x_val = builder.emit_literal(10, RueType::I32);
        let _x_let = builder.emit_let("x", x_val, RueType::I32);

        let y_val = builder.emit_literal(20, RueType::I32);
        let _y_let = builder.emit_let("y", y_val, RueType::I32);

        // Create condition
        let cond = builder.emit_literal(1, RueType::Bool);

        // Capture environment for passing to branches
        let incoming = builder.capture_environment();
        let order: Vec<BindingId> = incoming.iter().map(|(b, _, _, _)| *b).collect();

        // Create blocks with explicit parameters for captured environment
        let params: Vec<BlockParam> = incoming
            .iter()
            .map(|(_, n, _, ty)| BlockParam {
                name: builder.hir.strings.intern(n),
                ty: ty.clone(),
            })
            .collect();

        let then_block_id = builder.start_block_with_params(params.clone());
        builder.set_block_binding_order(then_block_id, order.clone());
        builder.end_block();

        let else_block_id = builder.start_block_with_params(params.clone());
        builder.set_block_binding_order(else_block_id, order.clone());
        builder.end_block();

        // Create merge block with a parameter to receive the result
        let merge_param = BlockParam {
            name: builder.hir.strings.intern("result"),
            ty: RueType::I32,
        };
        let merge_block_id = builder.start_block_with_params(vec![merge_param]);
        builder.end_block();

        // Set if terminator with explicit environment arguments
        let env_args: Vec<ValueRef> = incoming.iter().map(|(_, _, v, _)| *v).collect();
        builder.set_if_terminator(
            cond,
            then_block_id,
            env_args.clone(),
            else_block_id,
            env_args,
        );

        // Process then branch - x and y should be accessible
        builder.set_insertion_point(then_block_id);
        let x_in_then = builder.emit_load("x");
        assert!(x_in_then.is_ok(), "x should be accessible in then branch");
        let y_in_then = builder.emit_load("y");
        assert!(y_in_then.is_ok(), "y should be accessible in then branch");

        // Use the variables in some computation
        let sum = builder.emit_binary(
            x_in_then.unwrap(),
            y_in_then.unwrap(),
            BinOp::Add,
            RueType::I32,
        );
        builder.set_goto_terminator(merge_block_id, vec![sum]);

        // Process else branch - x and y should also be accessible
        builder.set_insertion_point(else_block_id);
        let x_in_else = builder.emit_load("x");
        assert!(x_in_else.is_ok(), "x should be accessible in else branch");
        let y_in_else = builder.emit_load("y");
        assert!(y_in_else.is_ok(), "y should be accessible in else branch");

        // Use the variables differently in else
        let diff = builder.emit_binary(
            y_in_else.unwrap(),
            x_in_else.unwrap(),
            BinOp::Sub,
            RueType::I32,
        );
        builder.set_goto_terminator(merge_block_id, vec![diff]);

        builder.end_block();
        let _hir = builder.finish().unwrap();
    }

    #[test]
    fn test_nested_scope_capture() {
        // Test that nested scopes are properly captured with explicit parameter passing
        let mut builder = HirBuilder::new();

        builder.start_block();

        // Outer variable
        let outer_val = builder.emit_literal(100, RueType::I32);
        let _outer_let = builder.emit_let("outer", outer_val, RueType::I32);

        // Inner block needs to receive outer as a parameter for proper SSA
        let inner_params = vec![BlockParam {
            name: builder.hir.strings.intern("outer"),
            ty: RueType::I32,
        }];
        let inner_block_id = builder.start_block_with_params(inner_params);

        // To pass outer_val to inner block, we need a goto terminator
        // But first, complete the inner block definition

        // Inner variable in this block
        let inner_val = builder.emit_literal(200, RueType::I32);
        let _inner_let = builder.emit_let("inner", inner_val, RueType::I32);

        // Create condition for nested if
        let cond = builder.emit_literal(0, RueType::Bool);

        // Create nested then/else blocks that will receive both outer and inner as parameters
        let nested_params = vec![
            BlockParam {
                name: builder.hir.strings.intern("outer"),
                ty: RueType::I32,
            },
            BlockParam {
                name: builder.hir.strings.intern("inner"),
                ty: RueType::I32,
            },
        ];

        let nested_then_id = builder.start_block_with_params(nested_params.clone());
        builder.end_block();

        let nested_else_id = builder.start_block_with_params(nested_params.clone());
        builder.end_block();

        // Access outer through the block parameter (it's the first parameter)
        let outer_param =
            ValueRef::from_block_param_with_id(inner_block_id, BlockParamIndex::new(0));

        // Set terminator with the values available in the inner block
        builder.set_if_terminator(
            cond,
            nested_then_id,
            vec![outer_param, inner_val], // Pass both values explicitly
            nested_else_id,
            vec![outer_param, inner_val], // Pass both values explicitly
        );

        // In nested then, both should be accessible via block parameters
        builder.set_insertion_point(nested_then_id);
        let outer_in_nested = builder.emit_load("outer");
        assert!(
            outer_in_nested.is_ok(),
            "outer should be accessible in nested then"
        );
        let inner_in_nested = builder.emit_load("inner");
        assert!(
            inner_in_nested.is_ok(),
            "inner should be accessible in nested then"
        );

        // End inner block
        builder.set_insertion_point(inner_block_id);
        builder.end_block();

        // Now set the goto from main block to inner block
        builder.set_goto_terminator(inner_block_id, vec![outer_val]);

        builder.end_block(); // End main block

        let _hir = builder.finish().unwrap();
    }

    // Tests for Structured While Loop Scaffolding

    #[test]
    fn test_while_ctx_creation() {
        // Test basic WhileCtx creation and accessor methods
        let mut builder = HirBuilder::new();

        builder.start_block();

        // Setup initial values for loop-carried variables
        let i_init = builder.emit_literal(0, RueType::I32);
        let sum_init = builder.emit_literal(0, RueType::I32);

        let initial_values = vec![
            ("i".to_string(), i_init, RueType::I32),
            ("sum".to_string(), sum_init, RueType::I32),
        ];

        // Convert to new format with BindingIds
        let carried_values: Vec<(BindingId, String, ValueRef, RueType)> = initial_values
            .into_iter()
            .map(|(name, value, ty)| (builder.alloc_binding_id(), name, value, ty))
            .collect();

        let ctx = builder.while_begin(carried_values);

        // Verify blocks exist in the arena
        assert!(builder.hir.block(ctx.header_block_id).is_some());
        assert!(builder.hir.block(ctx.body_block_id).is_some());
        assert!(builder.hir.block(ctx.exit_block_id).is_some());

        // Verify context structure
        assert_eq!(ctx.loop_carried_vars.len(), 2);
        // loop_carried_vars is now Vec<(BindingId, String, RueType)>
        assert_eq!(ctx.loop_carried_vars[0].1, "i");
        assert_eq!(ctx.loop_carried_vars[1].1, "sum");
        assert_eq!(ctx.loop_carried_vars[0].2, RueType::I32);
        assert_eq!(ctx.loop_carried_vars[1].2, RueType::I32);

        // Verify different block IDs were allocated
        assert_ne!(ctx.header_block_id, ctx.body_block_id);
        assert_ne!(ctx.body_block_id, ctx.exit_block_id);
        assert_ne!(ctx.header_block_id, ctx.exit_block_id);

        // Verify parameter generation
        let header_params = ctx.header_params(&mut builder.hir.strings);
        assert_eq!(header_params.len(), 2);
        assert_eq!(builder.hir.strings.get(header_params[0].name), "i");
        assert_eq!(builder.hir.strings.get(header_params[1].name), "sum");

        let exit_params = ctx.exit_params(&mut builder.hir.strings);
        assert_eq!(exit_params.len(), 2);
        assert_eq!(builder.hir.strings.get(exit_params[0].name), "i_final");
        assert_eq!(builder.hir.strings.get(exit_params[1].name), "sum_final");

        builder.end_block();
        let _hir = builder.finish().unwrap();
    }

    #[test]
    fn test_while_begin_creates_proper_structure() {
        // Test that while_begin creates the initial loop structure correctly
        let mut builder = HirBuilder::new();

        builder.start_block();

        // Initial values
        let counter_init = builder.emit_literal(0, RueType::I32);
        let initial_values = vec![("counter".to_string(), counter_init, RueType::I32)];

        // Convert to new format with BindingIds
        let carried_values: Vec<(BindingId, String, ValueRef, RueType)> = initial_values
            .into_iter()
            .map(|(name, value, ty)| (builder.alloc_binding_id(), name, value, ty))
            .collect();

        let ctx = builder.while_begin(carried_values);
        let main_block_id = builder.end_block();

        let hir = builder.finish().unwrap();

        // Verify main block has goto terminator to header
        let main_block = hir.block(main_block_id).unwrap();
        if let Terminator::Goto { target, args } = &main_block.term {
            assert_eq!(*target, ctx.header_block_id);
            assert_eq!(args.len(), 1);
            assert!(args[0].is_instruction());
        } else {
            panic!("Expected Goto terminator in main block");
        }

        // Verify header block was created with proper parameters
        let header_block = hir.block(ctx.header_block_id).unwrap();
        assert_eq!(header_block.params.len(), 1);
        assert_eq!(hir.strings.get(header_block.params[0].name), "counter");
        assert_eq!(header_block.params[0].ty, RueType::I32);

        // Verify body block was created with same parameters as header
        let body_block = hir.block(ctx.body_block_id).unwrap();
        assert_eq!(body_block.params.len(), 1); // Should have same parameters as header
        assert_eq!(body_block.body.len(), 0); // Should have empty body initially
    }

    #[test]
    fn test_while_set_cond_creates_branches() {
        // Test that while_set_cond properly sets up conditional branching
        let mut builder = HirBuilder::new();

        builder.start_block();

        let counter_init = builder.emit_literal(0, RueType::I32);
        let initial_values = vec![("counter".to_string(), counter_init, RueType::I32)];

        // Convert to new format with BindingIds
        let carried_values: Vec<(BindingId, String, ValueRef, RueType)> = initial_values
            .into_iter()
            .map(|(name, value, ty)| (builder.alloc_binding_id(), name, value, ty))
            .collect();

        let ctx = builder.while_begin(carried_values);

        // Set condition: counter < 10
        let header_values = builder.while_set_cond(&ctx, |builder, header_vals| {
            let ten = builder.emit_literal(10, RueType::I32);
            builder.emit_binary(header_vals[0], ten, BinOp::Lt, RueType::Bool)
        });

        // Verify we got header values
        assert_eq!(header_values.len(), 1);
        assert!(header_values[0].is_block_param());
        assert_eq!(header_values[0].block_id(), ctx.header_block_id);

        builder.end_block();
        let hir = builder.finish().unwrap();

        // Verify the arguments are what we expect
        let header_block = hir.block(ctx.header_block_id).unwrap();

        // Verify header block has conditional terminator
        if let Terminator::If {
            cond,
            then_target,
            else_target,
            then_args,
            else_args,
        } = &header_block.term
        {
            assert!(cond.is_instruction());
            assert_eq!(*then_target, ctx.body_block_id);
            assert_eq!(*else_target, ctx.exit_block_id);
            // The automatic environment capture is adding the header parameter to both branches
            // This is actually correct behavior for the while loop scaffolding
            assert_eq!(then_args.len(), 1); // One loop variable captured
            assert_eq!(else_args.len(), 1); // Same variable captured
        } else {
            panic!("Expected If terminator in header block");
        }

        // Verify header block has condition computation
        assert_eq!(header_block.body.len(), 2); // literal 10 + binary comparison
    }

    #[test]
    fn test_while_prepare_exit_patches_else_branch() {
        // Test that while_prepare_exit_from_header properly sets up exit block and patches else branch
        let mut builder = HirBuilder::new();

        builder.start_block();

        let counter_init = builder.emit_literal(0, RueType::I32);
        let initial_values = vec![("counter".to_string(), counter_init, RueType::I32)];

        // Convert to new format with BindingIds
        let carried_values: Vec<(BindingId, String, ValueRef, RueType)> = initial_values
            .into_iter()
            .map(|(name, value, ty)| (builder.alloc_binding_id(), name, value, ty))
            .collect();

        let mut ctx = builder.while_begin(carried_values);

        let header_values = builder.while_set_cond(&ctx, |builder, header_vals| {
            let ten = builder.emit_literal(10, RueType::I32);
            builder.emit_binary(header_vals[0], ten, BinOp::Lt, RueType::Bool)
        });

        // Prepare exit - this should patch the else branch
        builder.while_prepare_exit_from_header(&mut ctx, &header_values);

        builder.end_block();
        let hir = builder.finish().unwrap();

        // Verify exit block was created with parameters
        let exit_block = hir.block(ctx.exit_block_id).unwrap();
        assert_eq!(exit_block.params.len(), 1);
        assert_eq!(hir.strings.get(exit_block.params[0].name), "counter");
        assert_eq!(exit_block.params[0].ty, RueType::I32);

        // Verify header block's else branch was patched
        let header_block = hir.block(ctx.header_block_id).unwrap();
        if let Terminator::If {
            else_target,
            else_args,
            ..
        } = &header_block.term
        {
            assert_eq!(*else_target, ctx.exit_block_id);
            assert_eq!(else_args.len(), 1);
            assert!(else_args[0].is_block_param());
            assert_eq!(else_args[0].block_id(), ctx.header_block_id);
        } else {
            panic!("Expected If terminator in header block");
        }

        // Verify exit_prepared flag is set
        assert!(ctx.exit_prepared);
    }

    #[test]
    fn test_while_backedge_completes_loop() {
        // Test complete while loop construction with backedge
        let mut builder = HirBuilder::new();

        builder.start_block();

        let counter_init = builder.emit_literal(0, RueType::I32);
        let initial_values = vec![("counter".to_string(), counter_init, RueType::I32)];

        // Convert to new format with BindingIds
        let carried_values: Vec<(BindingId, String, ValueRef, RueType)> = initial_values
            .into_iter()
            .map(|(name, value, ty)| (builder.alloc_binding_id(), name, value, ty))
            .collect();

        let mut ctx = builder.while_begin(carried_values);

        let header_values = builder.while_set_cond(&ctx, |builder, header_vals| {
            let ten = builder.emit_literal(10, RueType::I32);
            builder.emit_binary(header_vals[0], ten, BinOp::Lt, RueType::Bool)
        });

        builder.while_prepare_exit_from_header(&mut ctx, &header_values);

        // Switch to body and add increment logic
        builder.set_insertion_point(ctx.body_block_id);
        // Body block receives loop variables as parameters due to environment capture
        let body_counter = builder.emit_load("counter").unwrap(); // Access via parameter name
        let one = builder.emit_literal(1, RueType::I32);
        let incremented_counter = builder.emit_binary(body_counter, one, BinOp::Add, RueType::I32);

        // Complete the loop with backedge
        builder.while_backedge(&ctx, vec![incremented_counter]);

        builder.end_block();
        let hir = builder.finish().unwrap();

        // Verify complete loop structure
        let header_block = hir.block(ctx.header_block_id).unwrap();
        let body_block = hir.block(ctx.body_block_id).unwrap();
        let exit_block = hir.block(ctx.exit_block_id).unwrap();

        // Header should branch to body/exit
        if let Terminator::If {
            then_target,
            else_target,
            ..
        } = &header_block.term
        {
            assert_eq!(*then_target, ctx.body_block_id);
            assert_eq!(*else_target, ctx.exit_block_id);
        } else {
            panic!("Expected If terminator in header");
        }

        // Body should have increment logic and goto header
        assert_eq!(body_block.body.len(), 2); // literal 1 + binary add
        if let Terminator::Goto { target, args } = &body_block.term {
            assert_eq!(*target, ctx.header_block_id);
            assert_eq!(args.len(), 1);
            assert!(args[0].is_instruction());
            assert_eq!(args[0].block_id(), ctx.body_block_id);
        } else {
            panic!("Expected Goto terminator in body");
        }

        // Exit should have parameters for final values
        assert_eq!(exit_block.params.len(), 1);
        assert_eq!(hir.strings.get(exit_block.params[0].name), "counter");
    }

    #[test]
    fn test_while_loop_with_multiple_loop_carried_vars() {
        // Test while loop with multiple loop-carried variables
        let mut builder = HirBuilder::new();

        builder.start_block();

        let i_init = builder.emit_literal(0, RueType::I32);
        let sum_init = builder.emit_literal(0, RueType::I32);
        let initial_values = vec![
            ("i".to_string(), i_init, RueType::I32),
            ("sum".to_string(), sum_init, RueType::I32),
        ];

        // Convert to new format with BindingIds
        let carried_values: Vec<(BindingId, String, ValueRef, RueType)> = initial_values
            .into_iter()
            .map(|(name, value, ty)| (builder.alloc_binding_id(), name, value, ty))
            .collect();

        let mut ctx = builder.while_begin(carried_values);

        let header_values = builder.while_set_cond(&ctx, |builder, header_vals| {
            let ten = builder.emit_literal(10, RueType::I32);
            builder.emit_binary(header_vals[0], ten, BinOp::Lt, RueType::Bool)
        });

        builder.while_prepare_exit_from_header(&mut ctx, &header_values);

        // Build body: sum += i; i += 1
        builder.set_insertion_point(ctx.body_block_id);
        // Body block receives loop variables as parameters due to environment capture
        let body_i = builder.emit_load("i").unwrap();
        let body_sum = builder.emit_load("sum").unwrap();
        let new_sum = builder.emit_binary(body_sum, body_i, BinOp::Add, RueType::I32);
        let one = builder.emit_literal(1, RueType::I32);
        let new_i = builder.emit_binary(body_i, one, BinOp::Add, RueType::I32);

        builder.while_backedge(&ctx, vec![new_i, new_sum]);

        builder.end_block();
        let hir = builder.finish().unwrap();

        // Verify parameter counts throughout
        let header_block = hir.block(ctx.header_block_id).unwrap();
        let body_block = hir.block(ctx.body_block_id).unwrap();
        let exit_block = hir.block(ctx.exit_block_id).unwrap();

        assert_eq!(header_block.params.len(), 2);
        assert_eq!(exit_block.params.len(), 2);

        // Verify terminator argument counts
        if let Terminator::If { else_args, .. } = &header_block.term {
            assert_eq!(else_args.len(), 2);
        }

        if let Terminator::Goto { args, .. } = &body_block.term {
            assert_eq!(args.len(), 2);
        }
    }

    #[test]
    #[should_panic(expected = "SSA violation")]
    fn test_while_backedge_validates_ssa() {
        // Test that while_backedge properly validates SSA form
        let mut builder = HirBuilder::new();

        builder.start_block();
        let counter_init = builder.emit_literal(0, RueType::I32);
        let initial_values = vec![("counter".to_string(), counter_init, RueType::I32)];

        // Convert to new format with BindingIds
        let carried_values: Vec<(BindingId, String, ValueRef, RueType)> = initial_values
            .into_iter()
            .map(|(name, value, ty)| (builder.alloc_binding_id(), name, value, ty))
            .collect();

        let mut ctx = builder.while_begin(carried_values);
        let header_values = builder.while_set_cond(&ctx, |builder, header_vals| {
            let ten = builder.emit_literal(10, RueType::I32);
            builder.emit_binary(header_vals[0], ten, BinOp::Lt, RueType::Bool)
        });

        builder.while_prepare_exit_from_header(&mut ctx, &header_values);

        // Try to use a value from header block in the body backedge
        // This should panic with SSA violation
        builder.set_insertion_point(ctx.body_block_id);
        builder.while_backedge(&ctx, vec![header_values[0]]);
    }

    #[test]
    #[should_panic(expected = "argument count mismatch")]
    fn test_while_backedge_validates_arity() {
        // Test that while_backedge validates argument count
        let mut builder = HirBuilder::new();

        builder.start_block();
        let counter_init = builder.emit_literal(0, RueType::I32);
        let initial_values = vec![("counter".to_string(), counter_init, RueType::I32)];

        // Convert to new format with BindingIds
        let carried_values: Vec<(BindingId, String, ValueRef, RueType)> = initial_values
            .into_iter()
            .map(|(name, value, ty)| (builder.alloc_binding_id(), name, value, ty))
            .collect();

        let ctx = builder.while_begin(carried_values);
        let _header_values = builder.while_set_cond(&ctx, |builder, header_vals| {
            let ten = builder.emit_literal(10, RueType::I32);
            builder.emit_binary(header_vals[0], ten, BinOp::Lt, RueType::Bool)
        });

        builder.set_insertion_point(ctx.body_block_id);
        let val1 = builder.emit_literal(1, RueType::I32);
        let val2 = builder.emit_literal(2, RueType::I32);

        // Try to pass wrong number of arguments (2 instead of 1)
        // This should panic with argument count mismatch
        builder.while_backedge(&ctx, vec![val1, val2]);
    }
}
