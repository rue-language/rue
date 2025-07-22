//! Mid-level Intermediate Representation (MIR)
//!
//! MIR is an SSA-form intermediate representation that sits between HIR and
//! platform-specific instructions. It uses block parameters instead of phi nodes
//! for a cleaner and more intuitive representation of control flow joins.
//!
//! ## Key Features
//!
//! - **SSA form**: Each value is assigned exactly once
//! - **Block parameters**: Control flow joins use parameters instead of phi nodes
//! - **Basic blocks**: Explicit control flow graph structure
//! - **Type preservation**: Full type information from HIR is maintained
//! - **Optimization-friendly**: Designed for analysis and transformation passes
//!
//! ## Example
//!
//! ```text
//! fn max(a: i32, b: i32) -> i32:
//!   B0(a: i32, b: i32):
//!     t0 = a > b
//!     branch t0, B1, B2
//!
//!   B1:
//!     goto B3(a)
//!
//!   B2:
//!     goto B3(b)
//!
//!   B3(result: i32):
//!     return result
//! ```

use crate::types::RueType;
use rue_lexer::Span;
use std::fmt;

/// A unique identifier for a basic block
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// A unique identifier for an SSA temporary
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Temp(pub u32);

/// A complete MIR program
#[derive(Debug, Clone, PartialEq)]
pub struct MirProgram {
    /// All functions in the program
    pub functions: Vec<MirFunction>,
}

/// A function in MIR form
#[derive(Debug, Clone, PartialEq)]
pub struct MirFunction {
    /// Function name
    pub name: String,
    /// Function parameters (these become parameters of the entry block)
    pub params: Vec<(String, RueType)>,
    /// Return type
    pub return_type: RueType,
    /// All basic blocks in the function
    pub blocks: Vec<BasicBlock>,
    /// The entry block ID (always the first block)
    pub entry_block: BlockId,
    /// Source span for debugging
    pub span: Span,
}

/// A basic block - a sequence of statements with a single entry and exit
#[derive(Debug, Clone, PartialEq)]
pub struct BasicBlock {
    /// Block identifier
    pub id: BlockId,
    /// Block parameters (for SSA form without phi nodes)
    pub params: Vec<(Temp, RueType)>,
    /// Statements executed in sequence
    pub statements: Vec<MirStatement>,
    /// Block terminator (control flow)
    pub terminator: MirTerminator,
}

/// A statement that doesn't transfer control
#[derive(Debug, Clone, PartialEq)]
pub enum MirStatement {
    /// Assignment of a value to a temporary
    Assign {
        /// Destination temporary
        dest: Temp,
        /// Source value
        value: MirValue,
        /// Source span for debugging
        span: Option<Span>,
    },
}

/// Values that can be used in MIR
#[derive(Debug, Clone, PartialEq)]
pub enum MirValue {
    /// Use of an existing temporary
    Use(Temp),
    /// Constant value
    Const(MirConst),
    /// Binary operation
    BinaryOp { op: MirBinOp, lhs: Temp, rhs: Temp },
    /// Unary operation
    UnaryOp { op: MirUnaryOp, operand: Temp },
    /// Function call
    Call {
        func: String,
        args: Vec<Temp>,
        return_type: RueType,
    },
}

/// Constant values in MIR
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MirConst {
    /// 32-bit integer constant
    Int32(i32),
    /// 64-bit integer constant
    Int64(i64),
    /// Boolean constant
    Bool(bool),
    /// Unit constant
    Unit,
}

/// Binary operations in MIR
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MirBinOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    // Comparison
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Unary operations in MIR
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MirUnaryOp {
    /// Negation
    Neg,
}

/// Block terminators - instructions that end a basic block
#[derive(Debug, Clone, PartialEq)]
pub enum MirTerminator {
    /// Unconditional jump with arguments
    Goto { target: BlockId, args: Vec<Temp> },
    /// Conditional branch with arguments for each target
    Branch {
        condition: Temp,
        then_block: BlockId,
        then_args: Vec<Temp>,
        else_block: BlockId,
        else_args: Vec<Temp>,
    },
    /// Function return
    Return { value: Option<Temp> },
}

// Helper methods

impl MirConst {
    /// Get the type of this constant
    pub fn ty(&self) -> RueType {
        match self {
            MirConst::Int32(_) => RueType::I32,
            MirConst::Int64(_) => RueType::I64,
            MirConst::Bool(_) => RueType::Bool,
            MirConst::Unit => RueType::Unit,
        }
    }
}

impl MirValue {
    /// Get the type of this value (requires type information for temps)
    pub fn ty(&self, temp_types: &impl Fn(Temp) -> RueType) -> RueType {
        match self {
            MirValue::Use(temp) => temp_types(*temp),
            MirValue::Const(c) => c.ty(),
            MirValue::BinaryOp { op, lhs, .. } => {
                use MirBinOp::*;
                match op {
                    // Arithmetic operations preserve type
                    Add | Sub | Mul | Div | Mod => temp_types(*lhs),
                    // Comparison operations return bool
                    Lt | Le | Gt | Ge | Eq | Ne => RueType::Bool,
                }
            }
            MirValue::UnaryOp { operand, .. } => temp_types(*operand),
            MirValue::Call { return_type, .. } => return_type.clone(),
        }
    }
}

// Display implementations for debugging

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "B{}", self.0)
    }
}

impl fmt::Display for Temp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t{}", self.0)
    }
}

impl fmt::Display for MirProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, func) in self.functions.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{func}")?;
        }
        Ok(())
    }
}

impl fmt::Display for MirFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Function signature
        write!(f, "fn {}(", self.name)?;
        for (i, (name, ty)) in self.params.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{name}: {ty}")?;
        }
        writeln!(f, ") -> {}:", self.return_type)?;

        // Basic blocks
        for block in &self.blocks {
            write!(f, "{block}")?;
        }

        Ok(())
    }
}

impl fmt::Display for BasicBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Block header with parameters
        write!(f, "  {}", self.id)?;
        if !self.params.is_empty() {
            write!(f, "(")?;
            for (i, (temp, ty)) in self.params.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{temp}: {ty}")?;
            }
            write!(f, ")")?;
        }
        writeln!(f, ":")?;

        // Statements
        for stmt in &self.statements {
            writeln!(f, "    {stmt}")?;
        }

        // Terminator
        writeln!(f, "    {}", self.terminator)?;

        Ok(())
    }
}

impl fmt::Display for MirStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirStatement::Assign { dest, value, .. } => {
                write!(f, "{dest} = {value}")
            }
        }
    }
}

impl fmt::Display for MirValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirValue::Use(temp) => write!(f, "{temp}"),
            MirValue::Const(c) => write!(f, "{c}"),
            MirValue::BinaryOp { op, lhs, rhs } => {
                write!(f, "{lhs} {op} {rhs}")
            }
            MirValue::UnaryOp { op, operand } => {
                write!(f, "{op}{operand}")
            }
            MirValue::Call { func, args, .. } => {
                write!(f, "{func}(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
        }
    }
}

impl fmt::Display for MirConst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirConst::Int32(n) => write!(f, "{n}_i32"),
            MirConst::Int64(n) => write!(f, "{n}_i64"),
            MirConst::Bool(b) => write!(f, "{b}"),
            MirConst::Unit => write!(f, "()"),
        }
    }
}

impl fmt::Display for MirBinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use MirBinOp::*;
        match self {
            Add => write!(f, "+"),
            Sub => write!(f, "-"),
            Mul => write!(f, "*"),
            Div => write!(f, "/"),
            Mod => write!(f, "%"),
            Lt => write!(f, "<"),
            Le => write!(f, "<="),
            Gt => write!(f, ">"),
            Ge => write!(f, ">="),
            Eq => write!(f, "=="),
            Ne => write!(f, "!="),
        }
    }
}

impl fmt::Display for MirUnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirUnaryOp::Neg => write!(f, "-"),
        }
    }
}

impl fmt::Display for MirTerminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MirTerminator::Goto { target, args } => {
                write!(f, "goto {target}")?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            MirTerminator::Branch {
                condition,
                then_block,
                then_args,
                else_block,
                else_args,
            } => {
                write!(f, "branch {condition}, {then_block}")?;
                if !then_args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in then_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, ")")?;
                }
                write!(f, ", {else_block}")?;
                if !else_args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in else_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            MirTerminator::Return { value } => {
                write!(f, "return")?;
                if let Some(val) = value {
                    write!(f, " {val}")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mir_display() {
        // Create a simple MIR function: fn add(a: i32, b: i32) -> i32 { a + b }
        let func = MirFunction {
            name: "add".to_string(),
            params: vec![
                ("a".to_string(), RueType::I32),
                ("b".to_string(), RueType::I32),
            ],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            span: Span::dummy(),
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

        let expected = "fn add(a: i32, b: i32) -> i32:\n  B0(t0: i32, t1: i32):\n    t2 = t0 + t1\n    return t2\n";
        assert_eq!(format!("{func}"), expected);
    }
}
