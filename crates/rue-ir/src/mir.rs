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

use crate::types::{FieldId, RueType, StructId};
use rue_lexer::Span;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// A unique identifier for a basic block
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// A unique identifier for an SSA temporary
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Temp(pub u32);

/// Function signature information
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionSignature {
    /// Parameter types in order
    pub param_types: Vec<RueType>,
    /// Return type
    pub return_type: RueType,
}

/// A complete MIR program
#[derive(Debug, Clone, PartialEq)]
pub struct MirProgram {
    /// All functions in the program
    pub functions: Vec<MirFunction>,
    /// Function signatures for all functions (builtin and user-defined)
    pub function_signatures: HashMap<String, FunctionSignature>,
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
    /// Type information for all temporaries in this function
    pub temp_types: std::collections::HashMap<Temp, RueType>,
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
        kind: CallKind,
    },
    /// Construct an aggregate value (struct, tuple, or array)
    ConstructAggregate {
        /// Type of the aggregate being constructed
        ty: RueType,
        /// Values for each field/element in order
        fields: Vec<Temp>,
    },
    /// Extract a field from an aggregate value
    GetField {
        /// The aggregate value to extract from
        base: Temp,
        /// Which field to extract
        field: FieldId,
    },
    /// Update a field in an aggregate value (creates new aggregate)
    SetField {
        /// The aggregate value to update
        base: Temp,
        /// Which field to update
        field: FieldId,
        /// New value for the field
        value: Temp,
    },
    /// Partial struct update (functional update syntax)
    /// Creates a new struct with some fields from base and some overridden
    ///
    /// **MVS Semantics**: This operation CONSUMES the base struct.
    /// The base temp is moved into the result and should not be used after
    /// this operation. This is important for move/ownership tracking.
    StructUpdate {
        /// The base struct to copy from (consumed by this operation)
        base: Temp,
        /// Fields to override (field_id, new_value)
        updates: Vec<(FieldId, Temp)>,
        /// Type of the struct (for validation)
        struct_type: StructId,
    },
    /// Dynamic array access with bounds checking
    /// Accesses an array element at a runtime-computed index
    DynamicArrayAccess {
        /// The array to access
        base: Temp,
        /// The index (computed at runtime)
        index: Temp,
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
    /// Aggregate constant (struct, tuple, or array)
    Aggregate {
        /// Type of the aggregate
        ty: RueType,
        /// Constant values for each field/element
        /// Uses Rc to allow cheap cloning
        fields: Rc<[MirConst]>,
    },
}

/// Binary operations in MIR
///
/// ## Overflow Semantics
///
/// Rue uses wrapping semantics for integer overflow:
/// - `Add`, `Sub`, `Mul`: Use wrapping arithmetic (overflow wraps around)
/// - `Div`, `Mod`: Use truncating division toward zero (Rust's default behavior)
///   - Division by zero panics at runtime
///   - For signed division: `MIN / -1` wraps to `MIN` (e.g., `i32::MIN / -1 = i32::MIN`)
///   - For signed modulo: `MIN % -1` returns `0` (follows Rust's behavior)
///
/// This matches Rust's default integer behavior and provides predictable
/// semantics across all target platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MirBinOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    /// Division (truncates toward zero, panics on division by zero)
    Div,
    /// Modulo/remainder (follows Rust semantics, panics on division by zero)
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
    /// Negation (only valid for i32 and i64 types)
    Neg,
}

/// Function call kind for tracking side effects and optimization opportunities
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallKind {
    /// Pure function call - no side effects, result only depends on arguments
    /// Can be eliminated if result is unused, can be moved across other pure operations
    Pure,
    /// Impure function call - may have side effects or depend on global state
    /// Cannot be eliminated even if result is unused, order must be preserved
    Impure,
}

impl MirUnaryOp {
    /// Check if this unary operation is valid for the given type
    pub fn is_valid_for_type(&self, ty: &RueType) -> bool {
        match self {
            MirUnaryOp::Neg => matches!(ty, RueType::I32 | RueType::I64),
        }
    }

    /// Get the result type of applying this unary operation to the given type
    /// Returns None if the operation is not valid for the type
    pub fn result_type(&self, operand_type: &RueType) -> Option<RueType> {
        if self.is_valid_for_type(operand_type) {
            Some(operand_type.clone())
        } else {
            None
        }
    }
}

/// Block terminators - instructions that end a basic block
#[derive(Debug, Clone, PartialEq)]
pub enum MirTerminator {
    /// Unconditional jump with arguments
    Goto {
        target: BlockId,
        args: Vec<Temp>,
        /// Source span for debugging and error reporting
        span: Option<Span>,
    },
    /// Conditional branch with arguments for each target
    Branch {
        condition: Temp,
        then_block: BlockId,
        then_args: Vec<Temp>,
        else_block: BlockId,
        else_args: Vec<Temp>,
        /// Source span for debugging and error reporting
        span: Option<Span>,
    },
    /// Multi-way branch based on integer value (enables jump table optimization)
    Switch {
        /// The value to switch on (must be integer type)
        discriminant: Temp,
        /// Mapping from integer values to target blocks with arguments
        /// Values must be unique and fit in target integer type
        targets: Vec<(i64, BlockId, Vec<Temp>)>,
        /// Default case if no targets match
        default: BlockId,
        /// Arguments for default block
        default_args: Vec<Temp>,
        /// Source span for debugging and error reporting
        span: Option<Span>,
    },
    /// Marks unreachable code (enables dead code analysis and optimization assumptions)
    /// This terminator should never be reached during execution
    Unreachable {
        /// Source span for debugging and error reporting
        span: Option<Span>,
    },
    /// Function return
    Return {
        value: Option<Temp>,
        /// Source span for debugging and error reporting
        span: Option<Span>,
    },
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
            MirConst::Aggregate { ty, .. } => ty.clone(),
        }
    }

    /// Create an aggregate constant from a vector of fields
    pub fn aggregate(ty: RueType, fields: Vec<MirConst>) -> Self {
        MirConst::Aggregate {
            ty,
            fields: fields.into(), // Vec<T> converts to Rc<[T]>
        }
    }

    /// Get fields of an aggregate (returns empty slice for non-aggregates)
    pub fn fields(&self) -> &[MirConst] {
        match self {
            MirConst::Aggregate { fields, .. } => fields,
            _ => &[],
        }
    }
}

impl MirValue {
    /// Get the type of this value (requires type information for temps)
    /// Note: For Call values, the type must be determined from the assignment context
    /// since we no longer store return_type directly in the Call variant.
    pub fn ty(&self, temp_types: &impl Fn(Temp) -> RueType) -> Option<RueType> {
        match self {
            MirValue::Use(temp) => Some(temp_types(*temp)),
            MirValue::Const(c) => Some(c.ty()),
            MirValue::BinaryOp { op, lhs, .. } => {
                use MirBinOp::*;
                match op {
                    // Arithmetic operations preserve type
                    Add | Sub | Mul | Div | Mod => Some(temp_types(*lhs)),
                    // Comparison operations return bool
                    Lt | Le | Gt | Ge | Eq | Ne => Some(RueType::Bool),
                }
            }
            MirValue::UnaryOp { op, operand } => {
                let operand_type = temp_types(*operand);
                op.result_type(&operand_type)
            }
            MirValue::Call { .. } => {
                // Call types must be determined from assignment context
                // The destination temp of the assignment contains the type
                None
            }
            MirValue::ConstructAggregate { ty, .. } => Some(ty.clone()),
            MirValue::GetField { base, field } => {
                let base_ty = temp_types(*base);
                // Extract field type from aggregate type
                match (&base_ty, field) {
                    (RueType::Tuple(types), FieldId::Index(idx)) => types.get(*idx).cloned(),
                    (RueType::Array(elem_ty, _), FieldId::Index(_)) => {
                        Some(elem_ty.as_ref().clone())
                    }
                    // For structs, we'd need the struct definition to get field type
                    // For now, return None (type must be determined from context)
                    _ => None,
                }
            }
            MirValue::SetField { base, .. } => {
                // SetField creates a new aggregate of the same type as base
                Some(temp_types(*base))
            }
            MirValue::StructUpdate { struct_type, .. } => {
                // StructUpdate creates a new struct of the given type
                Some(RueType::Struct(*struct_type))
            }
            MirValue::DynamicArrayAccess { base, .. } => {
                // Dynamic array access returns the element type of the array
                let base_ty = temp_types(*base);
                match base_ty {
                    RueType::Array(elem_ty, _) => Some(elem_ty.as_ref().clone()),
                    _ => None, // Error case - not an array
                }
            }
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
        // Display function signatures first if any exist
        if !self.function_signatures.is_empty() {
            writeln!(f, "// Function signatures:")?;
            let mut sigs: Vec<_> = self.function_signatures.iter().collect();
            sigs.sort_by_key(|(name, _)| *name);
            for (name, sig) in sigs {
                write!(f, "// {name}(")?;
                for (i, ty) in sig.param_types.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{ty}")?;
                }
                writeln!(f, ") -> {}", sig.return_type)?;
            }
            writeln!(f)?;
        }

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
            MirValue::ConstructAggregate { ty, fields } => {
                write!(f, "ConstructAggregate {ty} {{")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}")?;
                }
                write!(f, "}}")
            }
            MirValue::GetField { base, field } => {
                write!(f, "{base}.{field}")
            }
            MirValue::SetField { base, field, value } => {
                write!(f, "SetField {base}.{field} = {value}")
            }
            MirValue::StructUpdate {
                base,
                updates,
                struct_type,
            } => {
                write!(f, "StructUpdate({struct_type}) {{ ")?;
                for (i, (field, value)) in updates.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}: {value}")?;
                }
                write!(f, ", ..{base} }}")
            }
            MirValue::DynamicArrayAccess { base, index } => {
                write!(f, "{base}[{index}]")
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
            MirConst::Aggregate { ty, fields } => {
                write!(f, "const {ty} {{")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}")?;
                }
                write!(f, "}}")
            }
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
            MirTerminator::Goto { target, args, .. } => {
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
                ..
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
            MirTerminator::Switch {
                discriminant,
                targets,
                default,
                default_args,
                ..
            } => {
                write!(f, "switch {discriminant} ")?;
                if !targets.is_empty() {
                    write!(f, "[")?;
                    for (i, (value, block, args)) in targets.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{value}: {block}")?;
                        if !args.is_empty() {
                            write!(f, "(")?;
                            for (j, arg) in args.iter().enumerate() {
                                if j > 0 {
                                    write!(f, ", ")?;
                                }
                                write!(f, "{arg}")?;
                            }
                            write!(f, ")")?;
                        }
                    }
                    write!(f, "] ")?;
                }
                write!(f, "default {default}")?;
                if !default_args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in default_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            MirTerminator::Unreachable { .. } => {
                write!(f, "unreachable")
            }
            MirTerminator::Return { value, .. } => {
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
            temp_types: HashMap::new(),
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
                    span: None,
                },
            }],
        };

        let expected = "fn add(a: i32, b: i32) -> i32:\n  B0(t0: i32, t1: i32):\n    t2 = t0 + t1\n    return t2\n";
        assert_eq!(format!("{func}"), expected);
    }

    #[test]
    fn test_unaryop_type_validation() {
        use crate::types::RueType;

        // Test that negation is valid for numeric types
        assert!(MirUnaryOp::Neg.is_valid_for_type(&RueType::I32));
        assert!(MirUnaryOp::Neg.is_valid_for_type(&RueType::I64));

        // Test that negation is invalid for non-numeric types
        assert!(!MirUnaryOp::Neg.is_valid_for_type(&RueType::Bool));
        assert!(!MirUnaryOp::Neg.is_valid_for_type(&RueType::Unit));

        // Test result type computation
        assert_eq!(
            MirUnaryOp::Neg.result_type(&RueType::I32),
            Some(RueType::I32)
        );
        assert_eq!(
            MirUnaryOp::Neg.result_type(&RueType::I64),
            Some(RueType::I64)
        );
        assert_eq!(MirUnaryOp::Neg.result_type(&RueType::Bool), None);
        assert_eq!(MirUnaryOp::Neg.result_type(&RueType::Unit), None);
    }

    #[test]
    fn test_switch_terminator_display() {
        // Test Switch terminator display formatting
        let switch_term = MirTerminator::Switch {
            discriminant: Temp(0),
            targets: vec![
                (1, BlockId(1), vec![]),
                (2, BlockId(2), vec![Temp(1)]),
                (3, BlockId(3), vec![Temp(2), Temp(3)]),
            ],
            default: BlockId(4),
            default_args: vec![Temp(4)],
            span: None,
        };

        let expected = "switch t0 [1: B1, 2: B2(t1), 3: B3(t2, t3)] default B4(t4)";
        assert_eq!(format!("{switch_term}"), expected);
    }

    #[test]
    fn test_unreachable_terminator_display() {
        // Test Unreachable terminator display formatting
        let unreachable_term = MirTerminator::Unreachable { span: None };

        let expected = "unreachable";
        assert_eq!(format!("{unreachable_term}"), expected);
    }

    #[test]
    fn test_mir_function_with_switch() {
        // Create a MIR function with a switch statement
        let func = MirFunction {
            name: "test_switch".to_string(),
            params: vec![("x".to_string(), RueType::I32)],
            return_type: RueType::I32,
            entry_block: BlockId(0),
            temp_types: HashMap::new(),
            span: Span::dummy(),
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    params: vec![(Temp(0), RueType::I32)],
                    statements: vec![],
                    terminator: MirTerminator::Switch {
                        discriminant: Temp(0),
                        targets: vec![(1, BlockId(1), vec![]), (2, BlockId(2), vec![])],
                        default: BlockId(3),
                        default_args: vec![],
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(1),
                        value: MirValue::Const(MirConst::Int32(10)),
                        span: None,
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(1)),
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(2),
                        value: MirValue::Const(MirConst::Int32(20)),
                        span: None,
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(2)),
                        span: None,
                    },
                },
                BasicBlock {
                    id: BlockId(3),
                    params: vec![],
                    statements: vec![MirStatement::Assign {
                        dest: Temp(3),
                        value: MirValue::Const(MirConst::Int32(30)),
                        span: None,
                    }],
                    terminator: MirTerminator::Return {
                        value: Some(Temp(3)),
                        span: None,
                    },
                },
            ],
        };

        let output = format!("{func}");
        assert!(output.contains("switch t0"));
        assert!(output.contains("1: B1"));
        assert!(output.contains("2: B2"));
        assert!(output.contains("default B3"));
    }
}

// Include aggregate tests module
#[cfg(test)]
#[path = "mir_aggregate_tests.rs"]
mod mir_aggregate_tests;
