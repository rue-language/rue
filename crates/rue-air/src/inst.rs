//! AIR instruction definitions.
//!
//! Like RIR, instructions are stored densely and referenced by index.

use std::fmt;

use crate::types::{StructId, Type};
use rue_span::Span;

/// A reference to an instruction in the AIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AirRef(u32);

impl AirRef {
    #[inline]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// The complete AIR for a function.
#[derive(Debug, Default)]
pub struct Air {
    instructions: Vec<AirInst>,
    /// The return type of this function
    return_type: Type,
}

impl Air {
    /// Create a new empty AIR.
    pub fn new(return_type: Type) -> Self {
        Self {
            instructions: Vec::new(),
            return_type,
        }
    }

    /// Add an instruction and return its reference.
    pub fn add_inst(&mut self, inst: AirInst) -> AirRef {
        let index = self.instructions.len() as u32;
        self.instructions.push(inst);
        AirRef::from_raw(index)
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: AirRef) -> &AirInst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// The return type of this function.
    #[inline]
    pub fn return_type(&self) -> Type {
        self.return_type
    }

    /// The number of instructions.
    #[inline]
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Whether there are no instructions.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Iterate over all instructions with their references.
    pub fn iter(&self) -> impl Iterator<Item = (AirRef, &AirInst)> {
        self.instructions
            .iter()
            .enumerate()
            .map(|(i, inst)| (AirRef::from_raw(i as u32), inst))
    }
}

/// A single AIR instruction.
#[derive(Debug, Clone)]
pub struct AirInst {
    pub data: AirInstData,
    pub ty: Type,
    pub span: Span,
}

/// AIR instruction data - fully typed operations.
#[derive(Debug, Clone)]
pub enum AirInstData {
    /// Integer constant (typed)
    Const(i64),

    /// Boolean constant
    BoolConst(bool),

    // Binary arithmetic operations
    /// Addition
    Add(AirRef, AirRef),
    /// Subtraction
    Sub(AirRef, AirRef),
    /// Multiplication
    Mul(AirRef, AirRef),
    /// Division
    Div(AirRef, AirRef),
    /// Modulo
    Mod(AirRef, AirRef),

    // Comparison operations (return bool)
    /// Equality
    Eq(AirRef, AirRef),
    /// Inequality
    Ne(AirRef, AirRef),
    /// Less than
    Lt(AirRef, AirRef),
    /// Greater than
    Gt(AirRef, AirRef),
    /// Less than or equal
    Le(AirRef, AirRef),
    /// Greater than or equal
    Ge(AirRef, AirRef),

    // Logical operations (return bool)
    /// Logical AND
    And(AirRef, AirRef),
    /// Logical OR
    Or(AirRef, AirRef),

    // Unary operations
    /// Negation
    Neg(AirRef),
    /// Logical NOT
    Not(AirRef),

    // Control flow
    /// Conditional branch
    Branch {
        cond: AirRef,
        then_value: AirRef,
        else_value: Option<AirRef>,
    },

    /// While loop
    Loop { cond: AirRef, body: AirRef },

    /// Break: exits the innermost loop
    Break,

    /// Continue: jumps to the next iteration of the innermost loop
    Continue,

    // Variable operations
    /// Allocate local variable with initial value
    /// Returns the slot index
    Alloc {
        /// Local variable slot index (0, 1, 2, ...)
        slot: u32,
        /// Initial value
        init: AirRef,
    },

    /// Load value from local variable
    Load {
        /// Local variable slot index
        slot: u32,
    },

    /// Store value to local variable
    Store {
        /// Local variable slot index
        slot: u32,
        /// Value to store
        value: AirRef,
    },

    /// Return from function
    Ret(AirRef),

    /// Function call
    Call {
        /// Function name
        name: String,
        /// Argument AIR refs
        args: Vec<AirRef>,
    },

    /// Intrinsic call (e.g., @dbg)
    Intrinsic {
        /// Intrinsic name (without @)
        name: String,
        /// Argument AIR refs
        args: Vec<AirRef>,
    },

    /// Reference to a function parameter
    Param {
        /// Parameter index (0-based)
        index: u32,
    },

    /// Block expression with statements and final value.
    /// Used to group side-effect statements with their result value,
    /// enabling demand-driven lowering for short-circuit evaluation.
    Block {
        /// Side-effect statements to execute in order
        statements: Vec<AirRef>,
        /// The block's resulting value
        value: AirRef,
    },

    // Struct operations
    /// Create a new struct instance with initialized fields
    StructInit {
        /// The struct type being created
        struct_id: StructId,
        /// Field values in declaration order
        fields: Vec<AirRef>,
    },

    /// Load a field from a struct value
    FieldGet {
        /// The struct value
        base: AirRef,
        /// The struct type
        struct_id: StructId,
        /// Field index (0-based, in declaration order)
        field_index: u32,
    },

    /// Store a value to a struct field
    FieldSet {
        /// The struct variable slot
        slot: u32,
        /// The struct type
        struct_id: StructId,
        /// Field index (0-based, in declaration order)
        field_index: u32,
        /// Value to store
        value: AirRef,
    },
}

impl fmt::Display for AirRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

impl fmt::Display for Air {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "air (return_type: {}) {{", self.return_type.name())?;
        for (inst_ref, inst) in self.iter() {
            write!(f, "    {} : {} = ", inst_ref, inst.ty.name())?;
            match &inst.data {
                AirInstData::Const(v) => writeln!(f, "const {}", v)?,
                AirInstData::BoolConst(v) => writeln!(f, "const {}", v)?,
                AirInstData::Add(lhs, rhs) => writeln!(f, "add {}, {}", lhs, rhs)?,
                AirInstData::Sub(lhs, rhs) => writeln!(f, "sub {}, {}", lhs, rhs)?,
                AirInstData::Mul(lhs, rhs) => writeln!(f, "mul {}, {}", lhs, rhs)?,
                AirInstData::Div(lhs, rhs) => writeln!(f, "div {}, {}", lhs, rhs)?,
                AirInstData::Mod(lhs, rhs) => writeln!(f, "mod {}, {}", lhs, rhs)?,
                AirInstData::Eq(lhs, rhs) => writeln!(f, "eq {}, {}", lhs, rhs)?,
                AirInstData::Ne(lhs, rhs) => writeln!(f, "ne {}, {}", lhs, rhs)?,
                AirInstData::Lt(lhs, rhs) => writeln!(f, "lt {}, {}", lhs, rhs)?,
                AirInstData::Gt(lhs, rhs) => writeln!(f, "gt {}, {}", lhs, rhs)?,
                AirInstData::Le(lhs, rhs) => writeln!(f, "le {}, {}", lhs, rhs)?,
                AirInstData::Ge(lhs, rhs) => writeln!(f, "ge {}, {}", lhs, rhs)?,
                AirInstData::And(lhs, rhs) => writeln!(f, "and {}, {}", lhs, rhs)?,
                AirInstData::Or(lhs, rhs) => writeln!(f, "or {}, {}", lhs, rhs)?,
                AirInstData::Neg(operand) => writeln!(f, "neg {}", operand)?,
                AirInstData::Not(operand) => writeln!(f, "not {}", operand)?,
                AirInstData::Branch {
                    cond,
                    then_value,
                    else_value,
                } => {
                    if let Some(else_v) = else_value {
                        writeln!(f, "branch {}, {}, {}", cond, then_value, else_v)?
                    } else {
                        writeln!(f, "branch {}, {}", cond, then_value)?
                    }
                }
                AirInstData::Loop { cond, body } => writeln!(f, "loop {}, {}", cond, body)?,
                AirInstData::Break => writeln!(f, "break")?,
                AirInstData::Continue => writeln!(f, "continue")?,
                AirInstData::Alloc { slot, init } => writeln!(f, "alloc ${} = {}", slot, init)?,
                AirInstData::Load { slot } => writeln!(f, "load ${}", slot)?,
                AirInstData::Store { slot, value } => writeln!(f, "store ${} = {}", slot, value)?,
                AirInstData::Ret(inner) => writeln!(f, "ret {}", inner)?,
                AirInstData::Call { name, args } => {
                    write!(f, "call {}(", name)?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::Intrinsic { name, args } => {
                    write!(f, "intrinsic @{}(", name)?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::Param { index } => writeln!(f, "param {}", index)?,
                AirInstData::Block { statements, value } => {
                    write!(f, "block [")?;
                    for (i, s) in statements.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", s)?;
                    }
                    writeln!(f, "], {}", value)?;
                }
                AirInstData::StructInit { struct_id, fields } => {
                    write!(f, "struct_init #{} {{", struct_id.0)?;
                    for (i, field) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", field)?;
                    }
                    writeln!(f, "}}")?;
                }
                AirInstData::FieldGet {
                    base,
                    struct_id,
                    field_index,
                } => {
                    writeln!(f, "field_get {}.#{}.{}", base, struct_id.0, field_index)?;
                }
                AirInstData::FieldSet {
                    slot,
                    struct_id,
                    field_index,
                    value,
                } => {
                    writeln!(
                        f,
                        "field_set ${}.#{}.{} = {}",
                        slot, struct_id.0, field_index, value
                    )?;
                }
            }
        }
        writeln!(f, "}}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_air_ref_size() {
        assert_eq!(std::mem::size_of::<AirRef>(), 4);
    }

    #[test]
    fn test_add_and_get_inst() {
        let mut air = Air::new(Type::I32);
        let inst = AirInst {
            data: AirInstData::Const(42),
            ty: Type::I32,
            span: Span::new(0, 2),
        };
        let inst_ref = air.add_inst(inst);

        let retrieved = air.get(inst_ref);
        assert!(matches!(retrieved.data, AirInstData::Const(42)));
        assert_eq!(retrieved.ty, Type::I32);
    }
}
