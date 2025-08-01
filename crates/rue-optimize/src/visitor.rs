//! Visitor pattern for MIR transformations
//!
//! This module provides a flexible visitor pattern implementation for traversing
//! and transforming MIR (Mid-level Intermediate Representation) programs.
//!
//! ## Design Overview
//!
//! The visitor pattern provides three main traits:
//! - `MirVisitor`: For read-only analysis passes
//! - `MirMutVisitor`: For in-place mutation passes
//! - `MirFolder`: For structural transformation passes
//!
//! Each trait provides default implementations for traversing the entire MIR tree,
//! allowing passes to override only the methods they care about.

use rue_ir::mir::{
    BasicBlock, BlockId, MirConst, MirFunction, MirProgram, MirStatement, MirTerminator, MirValue,
    Temp,
};

/// Control flow for visitor traversal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitorControl {
    /// Continue normal traversal
    Continue,
    /// Skip children of current node but continue with siblings
    SkipChildren,
    /// Stop traversal entirely
    Break,
}

impl VisitorControl {
    /// Check if traversal should continue
    pub fn should_continue(&self) -> bool {
        matches!(
            self,
            VisitorControl::Continue | VisitorControl::SkipChildren
        )
    }

    /// Check if children should be visited
    pub fn should_visit_children(&self) -> bool {
        matches!(self, VisitorControl::Continue)
    }
}

/// Read-only visitor for MIR analysis passes
///
/// All methods have default implementations that perform the standard traversal.
/// Override only the methods you need for custom behavior.
///
/// IMPORTANT: If you override a method and want traversal to continue to child nodes,
/// you must either:
/// 1. Return the result of the corresponding walk_* function (recommended)
/// 2. Return VisitorControl::Continue if you don't need custom traversal logic
pub trait MirVisitor {
    /// Visit a program
    fn visit_program(&mut self, program: &MirProgram) -> VisitorControl {
        walk_program(self, program)
    }

    /// Visit a function
    fn visit_function(&mut self, func: &MirFunction) -> VisitorControl {
        walk_function(self, func)
    }

    /// Visit a basic block
    fn visit_block(&mut self, block: &BasicBlock) -> VisitorControl {
        walk_block(self, block)
    }

    /// Visit a statement
    fn visit_statement(&mut self, stmt: &MirStatement) -> VisitorControl {
        walk_statement(self, stmt)
    }

    /// Visit a terminator
    fn visit_terminator(&mut self, term: &MirTerminator) -> VisitorControl {
        walk_terminator(self, term)
    }

    /// Visit a value
    fn visit_value(&mut self, value: &MirValue) -> VisitorControl {
        walk_value(self, value)
    }

    /// Visit a temporary
    fn visit_temp(&mut self, _temp: &Temp) -> VisitorControl {
        VisitorControl::Continue
    }

    /// Visit a constant
    fn visit_const(&mut self, _constant: &MirConst) -> VisitorControl {
        VisitorControl::Continue
    }

    /// Visit a block ID
    fn visit_block_id(&mut self, _block_id: &BlockId) -> VisitorControl {
        VisitorControl::Continue
    }
}

/// Mutable visitor for MIR transformation passes
///
/// All methods have default implementations that perform the standard traversal.
/// Override only the methods you need for custom behavior.
///
/// IMPORTANT: If you override a method and want traversal to continue to child nodes,
/// you must either:
/// 1. Return the result of the corresponding walk_*_mut function (recommended)
/// 2. Return VisitorControl::Continue if you don't need custom traversal logic
///
/// Example:
/// ```ignore
/// impl MirMutVisitor for MyPass {
///     fn visit_function(&mut self, func: &mut MirFunction) -> VisitorControl {
///         // Do something before visiting children
///         self.prepare_for_function(func);
///         
///         // Continue traversal to blocks and statements
///         walk_function_mut(self, func)
///     }
///     
///     fn visit_temp(&mut self, temp: &mut Temp) -> VisitorControl {
///         // Leaf node - no children to visit
///         self.process_temp(temp);
///         VisitorControl::Continue
///     }
/// }
/// ```
pub trait MirMutVisitor {
    /// Visit a program
    fn visit_program(&mut self, program: &mut MirProgram) -> VisitorControl {
        walk_program_mut(self, program)
    }

    /// Visit a function
    fn visit_function(&mut self, func: &mut MirFunction) -> VisitorControl {
        walk_function_mut(self, func)
    }

    /// Visit a basic block
    fn visit_block(&mut self, block: &mut BasicBlock) -> VisitorControl {
        walk_block_mut(self, block)
    }

    /// Visit a statement
    fn visit_statement(&mut self, stmt: &mut MirStatement) -> VisitorControl {
        walk_statement_mut(self, stmt)
    }

    /// Visit a terminator
    fn visit_terminator(&mut self, term: &mut MirTerminator) -> VisitorControl {
        walk_terminator_mut(self, term)
    }

    /// Visit a value
    fn visit_value(&mut self, value: &mut MirValue) -> VisitorControl {
        walk_value_mut(self, value)
    }

    /// Visit a temporary
    fn visit_temp(&mut self, _temp: &mut Temp) -> VisitorControl {
        VisitorControl::Continue
    }

    /// Visit a constant
    fn visit_const(&mut self, _constant: &mut MirConst) -> VisitorControl {
        VisitorControl::Continue
    }

    /// Visit a block ID
    fn visit_block_id(&mut self, _block_id: &mut BlockId) -> VisitorControl {
        VisitorControl::Continue
    }
}

/// Folder for structural transformations
///
/// Unlike visitors, folders can change the structure of the MIR by:
/// - Deleting statements (by returning None)
/// - Replacing entire subtrees
/// - Changing types
pub trait MirFolder {
    /// Error type for folder operations
    type Error;

    /// Fold a program
    fn fold_program(&mut self, program: MirProgram) -> Result<MirProgram, Self::Error> {
        fold_program(self, program)
    }

    /// Fold a function
    fn fold_function(&mut self, func: MirFunction) -> Result<MirFunction, Self::Error> {
        fold_function(self, func)
    }

    /// Fold a basic block
    fn fold_block(&mut self, block: BasicBlock) -> Result<BasicBlock, Self::Error> {
        fold_block(self, block)
    }

    /// Fold a statement (can return None to delete)
    fn fold_statement(&mut self, stmt: MirStatement) -> Result<Option<MirStatement>, Self::Error> {
        fold_statement(self, stmt).map(Some)
    }

    /// Fold a terminator
    fn fold_terminator(&mut self, term: MirTerminator) -> Result<MirTerminator, Self::Error> {
        fold_terminator(self, term)
    }

    /// Fold a value
    fn fold_value(&mut self, value: MirValue) -> Result<MirValue, Self::Error> {
        fold_value(self, value)
    }

    /// Fold a temporary
    fn fold_temp(&mut self, temp: Temp) -> Result<Temp, Self::Error> {
        Ok(temp)
    }

    /// Fold a constant
    fn fold_const(&mut self, constant: MirConst) -> Result<MirConst, Self::Error> {
        Ok(constant)
    }

    /// Fold a block ID
    fn fold_block_id(&mut self, block_id: BlockId) -> Result<BlockId, Self::Error> {
        Ok(block_id)
    }
}

// Default walking implementations for read-only visitor

pub fn walk_program<V: MirVisitor + ?Sized>(
    visitor: &mut V,
    program: &MirProgram,
) -> VisitorControl {
    for func in &program.functions {
        let control = visitor.visit_function(func);
        if !control.should_continue() {
            return control;
        }
    }
    VisitorControl::Continue
}

pub fn walk_function<V: MirVisitor + ?Sized>(
    visitor: &mut V,
    func: &MirFunction,
) -> VisitorControl {
    for block in &func.blocks {
        let control = visitor.visit_block(block);
        if !control.should_continue() {
            return control;
        }
    }
    VisitorControl::Continue
}

pub fn walk_block<V: MirVisitor + ?Sized>(visitor: &mut V, block: &BasicBlock) -> VisitorControl {
    // Visit block parameters
    for (temp, _ty) in &block.params {
        let control = visitor.visit_temp(temp);
        if !control.should_continue() {
            return control;
        }
    }

    // Visit statements
    for stmt in &block.statements {
        let control = visitor.visit_statement(stmt);
        if !control.should_continue() {
            return control;
        }
    }

    // Visit terminator
    visitor.visit_terminator(&block.terminator)
}

pub fn walk_statement<V: MirVisitor + ?Sized>(
    visitor: &mut V,
    stmt: &MirStatement,
) -> VisitorControl {
    match stmt {
        MirStatement::Assign { dest, value, .. } => {
            let control = visitor.visit_temp(dest);
            if !control.should_continue() {
                return control;
            }
            visitor.visit_value(value)
        }
    }
}

pub fn walk_terminator<V: MirVisitor + ?Sized>(
    visitor: &mut V,
    term: &MirTerminator,
) -> VisitorControl {
    match term {
        MirTerminator::Return { value, .. } => {
            if let Some(temp) = value {
                visitor.visit_temp(temp)
            } else {
                VisitorControl::Continue
            }
        }
        MirTerminator::Goto { target, args, .. } => {
            let control = visitor.visit_block_id(target);
            if !control.should_continue() {
                return control;
            }
            for arg in args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            VisitorControl::Continue
        }
        MirTerminator::Branch {
            condition,
            then_block,
            else_block,
            then_args,
            else_args,
            ..
        } => {
            let control = visitor.visit_temp(condition);
            if !control.should_continue() {
                return control;
            }
            let control = visitor.visit_block_id(then_block);
            if !control.should_continue() {
                return control;
            }
            let control = visitor.visit_block_id(else_block);
            if !control.should_continue() {
                return control;
            }
            for arg in then_args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            for arg in else_args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            VisitorControl::Continue
        }
        MirTerminator::Switch {
            discriminant,
            targets,
            default,
            default_args,
            ..
        } => {
            let control = visitor.visit_temp(discriminant);
            if !control.should_continue() {
                return control;
            }
            for (_, target_block, target_args) in targets {
                let control = visitor.visit_block_id(target_block);
                if !control.should_continue() {
                    return control;
                }
                for arg in target_args {
                    let control = visitor.visit_temp(arg);
                    if !control.should_continue() {
                        return control;
                    }
                }
            }
            let control = visitor.visit_block_id(default);
            if !control.should_continue() {
                return control;
            }
            for arg in default_args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            VisitorControl::Continue
        }
        MirTerminator::Unreachable { .. } => VisitorControl::Continue,
    }
}

pub fn walk_value<V: MirVisitor + ?Sized>(visitor: &mut V, value: &MirValue) -> VisitorControl {
    match value {
        MirValue::Use(temp) => visitor.visit_temp(temp),
        MirValue::Const(constant) => visitor.visit_const(constant),
        MirValue::BinaryOp { lhs, rhs, .. } => {
            let control = visitor.visit_temp(lhs);
            if !control.should_continue() {
                return control;
            }
            visitor.visit_temp(rhs)
        }
        MirValue::UnaryOp { operand, .. } => visitor.visit_temp(operand),
        MirValue::Call { args, .. } => {
            for arg in args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            VisitorControl::Continue
        }
    }
}

// Default walking implementations for mutable visitor

pub fn walk_program_mut<V: MirMutVisitor + ?Sized>(
    visitor: &mut V,
    program: &mut MirProgram,
) -> VisitorControl {
    for func in &mut program.functions {
        let control = visitor.visit_function(func);
        if !control.should_continue() {
            return control;
        }
    }
    VisitorControl::Continue
}

pub fn walk_function_mut<V: MirMutVisitor + ?Sized>(
    visitor: &mut V,
    func: &mut MirFunction,
) -> VisitorControl {
    for block in &mut func.blocks {
        let control = visitor.visit_block(block);
        if !control.should_continue() {
            return control;
        }
    }
    VisitorControl::Continue
}

pub fn walk_block_mut<V: MirMutVisitor + ?Sized>(
    visitor: &mut V,
    block: &mut BasicBlock,
) -> VisitorControl {
    // Visit block parameters
    for (temp, _ty) in &mut block.params {
        let control = visitor.visit_temp(temp);
        if !control.should_continue() {
            return control;
        }
    }

    // Visit statements
    for stmt in &mut block.statements {
        let control = visitor.visit_statement(stmt);
        if !control.should_continue() {
            return control;
        }
    }

    // Visit terminator
    visitor.visit_terminator(&mut block.terminator)
}

pub fn walk_statement_mut<V: MirMutVisitor + ?Sized>(
    visitor: &mut V,
    stmt: &mut MirStatement,
) -> VisitorControl {
    match stmt {
        MirStatement::Assign { dest, value, .. } => {
            let control = visitor.visit_temp(dest);
            if !control.should_continue() {
                return control;
            }
            visitor.visit_value(value)
        }
    }
}

pub fn walk_terminator_mut<V: MirMutVisitor + ?Sized>(
    visitor: &mut V,
    term: &mut MirTerminator,
) -> VisitorControl {
    match term {
        MirTerminator::Return { value, .. } => {
            if let Some(temp) = value {
                visitor.visit_temp(temp)
            } else {
                VisitorControl::Continue
            }
        }
        MirTerminator::Goto { target, args, .. } => {
            let control = visitor.visit_block_id(target);
            if !control.should_continue() {
                return control;
            }
            for arg in args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            VisitorControl::Continue
        }
        MirTerminator::Branch {
            condition,
            then_block,
            else_block,
            then_args,
            else_args,
            ..
        } => {
            let control = visitor.visit_temp(condition);
            if !control.should_continue() {
                return control;
            }
            let control = visitor.visit_block_id(then_block);
            if !control.should_continue() {
                return control;
            }
            let control = visitor.visit_block_id(else_block);
            if !control.should_continue() {
                return control;
            }
            for arg in then_args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            for arg in else_args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            VisitorControl::Continue
        }
        MirTerminator::Switch {
            discriminant,
            targets,
            default,
            default_args,
            ..
        } => {
            let control = visitor.visit_temp(discriminant);
            if !control.should_continue() {
                return control;
            }
            for (_, target_block, target_args) in targets {
                let control = visitor.visit_block_id(target_block);
                if !control.should_continue() {
                    return control;
                }
                for arg in target_args {
                    let control = visitor.visit_temp(arg);
                    if !control.should_continue() {
                        return control;
                    }
                }
            }
            let control = visitor.visit_block_id(default);
            if !control.should_continue() {
                return control;
            }
            for arg in default_args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            VisitorControl::Continue
        }
        MirTerminator::Unreachable { .. } => VisitorControl::Continue,
    }
}

pub fn walk_value_mut<V: MirMutVisitor + ?Sized>(
    visitor: &mut V,
    value: &mut MirValue,
) -> VisitorControl {
    match value {
        MirValue::Use(temp) => visitor.visit_temp(temp),
        MirValue::Const(constant) => visitor.visit_const(constant),
        MirValue::BinaryOp { lhs, rhs, .. } => {
            let control = visitor.visit_temp(lhs);
            if !control.should_continue() {
                return control;
            }
            visitor.visit_temp(rhs)
        }
        MirValue::UnaryOp { operand, .. } => visitor.visit_temp(operand),
        MirValue::Call { args, .. } => {
            for arg in args {
                let control = visitor.visit_temp(arg);
                if !control.should_continue() {
                    return control;
                }
            }
            VisitorControl::Continue
        }
    }
}

// Default folding implementations

pub fn fold_program<F: MirFolder + ?Sized>(
    folder: &mut F,
    mut program: MirProgram,
) -> Result<MirProgram, F::Error> {
    let mut functions = Vec::with_capacity(program.functions.len());
    for func in program.functions {
        functions.push(folder.fold_function(func)?);
    }
    program.functions = functions;
    Ok(program)
}

pub fn fold_function<F: MirFolder + ?Sized>(
    folder: &mut F,
    mut func: MirFunction,
) -> Result<MirFunction, F::Error> {
    let mut blocks = Vec::with_capacity(func.blocks.len());
    for block in func.blocks {
        blocks.push(folder.fold_block(block)?);
    }
    func.blocks = blocks;
    Ok(func)
}

pub fn fold_block<F: MirFolder + ?Sized>(
    folder: &mut F,
    mut block: BasicBlock,
) -> Result<BasicBlock, F::Error> {
    // Fold block parameters
    for (temp, _ty) in &mut block.params {
        *temp = folder.fold_temp(*temp)?;
    }

    // Fold statements (can filter out None results)
    let mut statements = Vec::new();
    for stmt in block.statements {
        if let Some(stmt) = folder.fold_statement(stmt)? {
            statements.push(stmt);
        }
    }
    block.statements = statements;

    // Fold terminator
    block.terminator = folder.fold_terminator(block.terminator)?;

    Ok(block)
}

pub fn fold_statement<F: MirFolder + ?Sized>(
    folder: &mut F,
    mut stmt: MirStatement,
) -> Result<MirStatement, F::Error> {
    match &mut stmt {
        MirStatement::Assign { dest, value, .. } => {
            *dest = folder.fold_temp(*dest)?;
            *value = folder.fold_value(value.clone())?;
        }
    }
    Ok(stmt)
}

pub fn fold_terminator<F: MirFolder + ?Sized>(
    folder: &mut F,
    mut term: MirTerminator,
) -> Result<MirTerminator, F::Error> {
    match &mut term {
        MirTerminator::Return { value, .. } => {
            if let Some(temp) = value {
                *temp = folder.fold_temp(*temp)?;
            }
        }
        MirTerminator::Goto { target, args, .. } => {
            *target = folder.fold_block_id(*target)?;
            for arg in args {
                *arg = folder.fold_temp(*arg)?;
            }
        }
        MirTerminator::Branch {
            condition,
            then_block,
            else_block,
            then_args,
            else_args,
            ..
        } => {
            *condition = folder.fold_temp(*condition)?;
            *then_block = folder.fold_block_id(*then_block)?;
            *else_block = folder.fold_block_id(*else_block)?;
            for arg in then_args {
                *arg = folder.fold_temp(*arg)?;
            }
            for arg in else_args {
                *arg = folder.fold_temp(*arg)?;
            }
        }
        MirTerminator::Switch {
            discriminant,
            targets,
            default,
            default_args,
            ..
        } => {
            *discriminant = folder.fold_temp(*discriminant)?;
            for (_, target_block, target_args) in targets {
                *target_block = folder.fold_block_id(*target_block)?;
                for arg in target_args {
                    *arg = folder.fold_temp(*arg)?;
                }
            }
            *default = folder.fold_block_id(*default)?;
            for arg in default_args {
                *arg = folder.fold_temp(*arg)?;
            }
        }
        MirTerminator::Unreachable { .. } => {}
    }
    Ok(term)
}

pub fn fold_value<F: MirFolder + ?Sized>(
    folder: &mut F,
    value: MirValue,
) -> Result<MirValue, F::Error> {
    Ok(match value {
        MirValue::Use(temp) => MirValue::Use(folder.fold_temp(temp)?),
        MirValue::Const(constant) => MirValue::Const(folder.fold_const(constant)?),
        MirValue::BinaryOp { op, lhs, rhs } => MirValue::BinaryOp {
            op,
            lhs: folder.fold_temp(lhs)?,
            rhs: folder.fold_temp(rhs)?,
        },
        MirValue::UnaryOp { op, operand } => MirValue::UnaryOp {
            op,
            operand: folder.fold_temp(operand)?,
        },
        MirValue::Call { func, args, kind } => {
            let mut new_args = Vec::with_capacity(args.len());
            for arg in args {
                new_args.push(folder.fold_temp(arg)?);
            }
            MirValue::Call {
                func,
                args: new_args,
                kind,
            }
        }
    })
}
