//! RIR instruction definitions.
//!
//! Instructions are stored in a dense array and referenced by index.
//! This provides good cache locality and efficient traversal.

use std::fmt;

use rue_intern::Symbol;
use rue_span::Span;

/// A reference to an instruction in the RIR.
///
/// This is a lightweight handle (4 bytes) that indexes into the instruction array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstRef(u32);

impl InstRef {
    /// Create an instruction reference from a raw index.
    #[inline]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Get the raw index.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// A pattern in a match expression (RIR level - untyped).
#[derive(Debug, Clone)]
pub enum RirPattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard(Span),
    /// Integer literal pattern
    Int(u64, Span),
    /// Boolean literal pattern
    Bool(bool, Span),
    /// Path pattern for enum variants (e.g., Color::Red)
    Path {
        /// The enum type name
        type_name: Symbol,
        /// The variant name
        variant: Symbol,
        /// Span of the pattern
        span: Span,
    },
}

impl RirPattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            RirPattern::Wildcard(span) => *span,
            RirPattern::Int(_, span) => *span,
            RirPattern::Bool(_, span) => *span,
            RirPattern::Path { span, .. } => *span,
        }
    }
}

/// The complete RIR for a source file.
#[derive(Debug, Default)]
pub struct Rir {
    /// All instructions in the file
    instructions: Vec<Inst>,
    /// Extra data for variable-length instruction payloads
    extra: Vec<u32>,
}

impl Rir {
    /// Create a new empty RIR.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an instruction and return its reference.
    pub fn add_inst(&mut self, inst: Inst) -> InstRef {
        let index = self.instructions.len() as u32;
        self.instructions.push(inst);
        InstRef::from_raw(index)
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: InstRef) -> &Inst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// Get a mutable reference to an instruction.
    #[inline]
    pub fn get_mut(&mut self, inst_ref: InstRef) -> &mut Inst {
        &mut self.instructions[inst_ref.0 as usize]
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
    pub fn iter(&self) -> impl Iterator<Item = (InstRef, &Inst)> {
        self.instructions
            .iter()
            .enumerate()
            .map(|(i, inst)| (InstRef::from_raw(i as u32), inst))
    }

    /// Add extra data and return the start index.
    pub fn add_extra(&mut self, data: &[u32]) -> u32 {
        let start = self.extra.len() as u32;
        self.extra.extend_from_slice(data);
        start
    }

    /// Get extra data by index.
    #[inline]
    pub fn get_extra(&self, start: u32, len: u32) -> &[u32] {
        let start = start as usize;
        let end = start + len as usize;
        &self.extra[start..end]
    }
}

/// A single RIR instruction.
#[derive(Debug, Clone)]
pub struct Inst {
    pub data: InstData,
    pub span: Span,
}

/// Instruction data - the actual operation.
#[derive(Debug, Clone)]
pub enum InstData {
    /// Integer constant
    IntConst(u64),

    /// Boolean constant
    BoolConst(bool),

    /// String constant (interned string content)
    StringConst(Symbol),

    /// Unit constant (for blocks that produce unit type)
    UnitConst,

    // Binary arithmetic operations
    /// Addition: lhs + rhs
    Add { lhs: InstRef, rhs: InstRef },
    /// Subtraction: lhs - rhs
    Sub { lhs: InstRef, rhs: InstRef },
    /// Multiplication: lhs * rhs
    Mul { lhs: InstRef, rhs: InstRef },
    /// Division: lhs / rhs
    Div { lhs: InstRef, rhs: InstRef },
    /// Modulo: lhs % rhs
    Mod { lhs: InstRef, rhs: InstRef },

    // Comparison operations
    /// Equality: lhs == rhs
    Eq { lhs: InstRef, rhs: InstRef },
    /// Inequality: lhs != rhs
    Ne { lhs: InstRef, rhs: InstRef },
    /// Less than: lhs < rhs
    Lt { lhs: InstRef, rhs: InstRef },
    /// Greater than: lhs > rhs
    Gt { lhs: InstRef, rhs: InstRef },
    /// Less than or equal: lhs <= rhs
    Le { lhs: InstRef, rhs: InstRef },
    /// Greater than or equal: lhs >= rhs
    Ge { lhs: InstRef, rhs: InstRef },

    // Logical operations
    /// Logical AND: lhs && rhs
    And { lhs: InstRef, rhs: InstRef },
    /// Logical OR: lhs || rhs
    Or { lhs: InstRef, rhs: InstRef },

    // Bitwise operations
    /// Bitwise AND: lhs & rhs
    BitAnd { lhs: InstRef, rhs: InstRef },
    /// Bitwise OR: lhs | rhs
    BitOr { lhs: InstRef, rhs: InstRef },
    /// Bitwise XOR: lhs ^ rhs
    BitXor { lhs: InstRef, rhs: InstRef },
    /// Left shift: lhs << rhs
    Shl { lhs: InstRef, rhs: InstRef },
    /// Right shift: lhs >> rhs (arithmetic for signed, logical for unsigned)
    Shr { lhs: InstRef, rhs: InstRef },

    // Unary operations
    /// Negation: -operand
    Neg { operand: InstRef },
    /// Logical NOT: !operand
    Not { operand: InstRef },
    /// Bitwise NOT: ~operand
    BitNot { operand: InstRef },

    // Control flow
    /// Branch: if cond then then_block else else_block
    Branch {
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
    },

    /// While loop: while cond { body }
    Loop { cond: InstRef, body: InstRef },

    /// Infinite loop: loop { body }
    InfiniteLoop { body: InstRef },

    /// Match expression: match scrutinee { pattern => expr, ... }
    Match {
        /// The value being matched
        scrutinee: InstRef,
        /// Match arms: [(pattern, body), ...]
        arms: Vec<(RirPattern, InstRef)>,
    },

    /// Break: exits the innermost loop
    Break,

    /// Continue: jumps to the next iteration of the innermost loop
    Continue,

    /// Function definition
    /// Contains: name symbol, parameters, return type symbol, body instruction ref
    FnDecl {
        name: Symbol,
        /// Parameter symbols and their type symbols: [(param_name, param_type), ...]
        params: Vec<(Symbol, Symbol)>,
        return_type: Symbol,
        body: InstRef,
    },

    /// Function call
    Call {
        /// Function name
        name: Symbol,
        /// Argument instruction refs
        args: Vec<InstRef>,
    },

    /// Intrinsic call (e.g., @dbg)
    Intrinsic {
        /// Intrinsic name (without @)
        name: Symbol,
        /// Argument instruction refs
        args: Vec<InstRef>,
    },

    /// Reference to a function parameter
    ParamRef {
        /// Parameter index (0-based)
        index: u32,
        /// Parameter name (for error messages)
        name: Symbol,
    },

    /// Return value from function (None for `return;` in unit-returning functions)
    Ret(Option<InstRef>),

    /// Block of instructions (for function bodies)
    /// The result is the last instruction in the block
    Block {
        /// Index into extra data where instruction refs start
        extra_start: u32,
        /// Number of instructions in the block
        len: u32,
    },

    // Variable operations
    /// Local variable declaration: allocates storage and initializes
    /// If name is None, this is a wildcard pattern that discards the value
    Alloc {
        /// Variable name (None for wildcard `_` pattern that discards the value)
        name: Option<Symbol>,
        /// Whether the variable is mutable
        is_mut: bool,
        /// Optional type annotation
        ty: Option<Symbol>,
        /// Initial value instruction
        init: InstRef,
    },

    /// Variable reference: reads the value of a variable
    VarRef {
        /// Variable name
        name: Symbol,
    },

    /// Assignment: stores a value into a mutable variable
    Assign {
        /// Variable name
        name: Symbol,
        /// Value to store
        value: InstRef,
    },

    // Struct operations
    /// Struct type declaration
    StructDecl {
        /// Struct name
        name: Symbol,
        /// Fields: [(field_name, field_type), ...]
        fields: Vec<(Symbol, Symbol)>,
    },

    /// Struct literal: creates a new struct instance
    StructInit {
        /// Struct type name
        type_name: Symbol,
        /// Field initializers: [(field_name, value_inst), ...]
        fields: Vec<(Symbol, InstRef)>,
    },

    /// Field access: reads a field from a struct
    FieldGet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Symbol,
    },

    /// Field assignment: writes a value to a struct field
    FieldSet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Symbol,
        /// Value to store
        value: InstRef,
    },

    // Enum operations
    /// Enum type declaration
    EnumDecl {
        /// Enum name
        name: Symbol,
        /// Variant names (no data for now)
        variants: Vec<Symbol>,
    },

    /// Enum variant: creates a value of an enum type
    EnumVariant {
        /// Enum type name
        type_name: Symbol,
        /// Variant name
        variant: Symbol,
    },

    // Array operations
    /// Array literal: creates a new array from element values
    ArrayInit {
        /// Element values
        elements: Vec<InstRef>,
    },

    /// Array index read: reads an element from an array
    IndexGet {
        /// Base array value
        base: InstRef,
        /// Index expression
        index: InstRef,
    },

    /// Array index write: writes a value to an array element
    IndexSet {
        /// Base array value (must be a VarRef to a mutable variable)
        base: InstRef,
        /// Index expression
        index: InstRef,
        /// Value to store
        value: InstRef,
    },
}

impl fmt::Display for InstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Printer for RIR that resolves symbols to their string values.
pub struct RirPrinter<'a, 'b> {
    rir: &'a Rir,
    interner: &'b rue_intern::Interner,
}

impl<'a, 'b> RirPrinter<'a, 'b> {
    /// Create a new RIR printer.
    pub fn new(rir: &'a Rir, interner: &'b rue_intern::Interner) -> Self {
        Self { rir, interner }
    }

    /// Format the RIR as a string.
    pub fn to_string(&self) -> String {
        let mut out = String::new();
        for (inst_ref, inst) in self.rir.iter() {
            out.push_str(&format!("{} = ", inst_ref));
            match &inst.data {
                InstData::IntConst(v) => {
                    out.push_str(&format!("const {}\n", v));
                }
                InstData::BoolConst(v) => {
                    out.push_str(&format!("const {}\n", v));
                }
                InstData::StringConst(s) => {
                    out.push_str(&format!("const {:?}\n", self.interner.get(*s)));
                }
                InstData::UnitConst => {
                    out.push_str("const ()\n");
                }
                InstData::Add { lhs, rhs } => {
                    out.push_str(&format!("add {}, {}\n", lhs, rhs));
                }
                InstData::Sub { lhs, rhs } => {
                    out.push_str(&format!("sub {}, {}\n", lhs, rhs));
                }
                InstData::Mul { lhs, rhs } => {
                    out.push_str(&format!("mul {}, {}\n", lhs, rhs));
                }
                InstData::Div { lhs, rhs } => {
                    out.push_str(&format!("div {}, {}\n", lhs, rhs));
                }
                InstData::Mod { lhs, rhs } => {
                    out.push_str(&format!("mod {}, {}\n", lhs, rhs));
                }
                InstData::Eq { lhs, rhs } => {
                    out.push_str(&format!("eq {}, {}\n", lhs, rhs));
                }
                InstData::Ne { lhs, rhs } => {
                    out.push_str(&format!("ne {}, {}\n", lhs, rhs));
                }
                InstData::Lt { lhs, rhs } => {
                    out.push_str(&format!("lt {}, {}\n", lhs, rhs));
                }
                InstData::Gt { lhs, rhs } => {
                    out.push_str(&format!("gt {}, {}\n", lhs, rhs));
                }
                InstData::Le { lhs, rhs } => {
                    out.push_str(&format!("le {}, {}\n", lhs, rhs));
                }
                InstData::Ge { lhs, rhs } => {
                    out.push_str(&format!("ge {}, {}\n", lhs, rhs));
                }
                InstData::And { lhs, rhs } => {
                    out.push_str(&format!("and {}, {}\n", lhs, rhs));
                }
                InstData::Or { lhs, rhs } => {
                    out.push_str(&format!("or {}, {}\n", lhs, rhs));
                }
                InstData::BitAnd { lhs, rhs } => {
                    out.push_str(&format!("bit_and {}, {}\n", lhs, rhs));
                }
                InstData::BitOr { lhs, rhs } => {
                    out.push_str(&format!("bit_or {}, {}\n", lhs, rhs));
                }
                InstData::BitXor { lhs, rhs } => {
                    out.push_str(&format!("bit_xor {}, {}\n", lhs, rhs));
                }
                InstData::Shl { lhs, rhs } => {
                    out.push_str(&format!("shl {}, {}\n", lhs, rhs));
                }
                InstData::Shr { lhs, rhs } => {
                    out.push_str(&format!("shr {}, {}\n", lhs, rhs));
                }
                InstData::Neg { operand } => {
                    out.push_str(&format!("neg {}\n", operand));
                }
                InstData::Not { operand } => {
                    out.push_str(&format!("not {}\n", operand));
                }
                InstData::BitNot { operand } => {
                    out.push_str(&format!("bit_not {}\n", operand));
                }
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    if let Some(else_b) = else_block {
                        out.push_str(&format!("branch {}, {}, {}\n", cond, then_block, else_b));
                    } else {
                        out.push_str(&format!("branch {}, {}\n", cond, then_block));
                    }
                }
                InstData::Loop { cond, body } => {
                    out.push_str(&format!("loop {}, {}\n", cond, body));
                }
                InstData::InfiniteLoop { body } => {
                    out.push_str(&format!("infinite_loop {}\n", body));
                }
                InstData::Match { scrutinee, arms } => {
                    let arms_str: Vec<String> = arms
                        .iter()
                        .map(|(pat, body)| {
                            let pat_str = match pat {
                                RirPattern::Wildcard(_) => "_".to_string(),
                                RirPattern::Int(n, _) => n.to_string(),
                                RirPattern::Bool(b, _) => b.to_string(),
                                RirPattern::Path {
                                    type_name, variant, ..
                                } => {
                                    format!(
                                        "{}::{}",
                                        self.interner.get(*type_name),
                                        self.interner.get(*variant)
                                    )
                                }
                            };
                            format!("{} => {}", pat_str, body)
                        })
                        .collect();
                    out.push_str(&format!(
                        "match {} {{ {} }}\n",
                        scrutinee,
                        arms_str.join(", ")
                    ));
                }
                InstData::Break => {
                    out.push_str("break\n");
                }
                InstData::Continue => {
                    out.push_str("continue\n");
                }
                InstData::FnDecl {
                    name,
                    params,
                    return_type,
                    body,
                } => {
                    let name_str = self.interner.get(*name);
                    let ret_str = self.interner.get(*return_type);
                    let params_str: Vec<String> = params
                        .iter()
                        .map(|(pname, ptype)| {
                            format!(
                                "{}: {}",
                                self.interner.get(*pname),
                                self.interner.get(*ptype)
                            )
                        })
                        .collect();
                    out.push_str(&format!(
                        "fn {}({}) -> {} {{\n",
                        name_str,
                        params_str.join(", "),
                        ret_str
                    ));
                    out.push_str(&format!("    {}\n", body));
                    out.push_str("}\n");
                }
                InstData::Ret(inner) => {
                    if let Some(inner) = inner {
                        out.push_str(&format!("ret {}\n", inner));
                    } else {
                        out.push_str("ret\n");
                    }
                }
                InstData::Call { name, args } => {
                    let name_str = self.interner.get(*name);
                    let args_str: Vec<String> = args.iter().map(|a| format!("{}", a)).collect();
                    out.push_str(&format!("call {}({})\n", name_str, args_str.join(", ")));
                }
                InstData::Intrinsic { name, args } => {
                    let name_str = self.interner.get(*name);
                    let args_str: Vec<String> = args.iter().map(|a| format!("{}", a)).collect();
                    out.push_str(&format!(
                        "intrinsic @{}({})\n",
                        name_str,
                        args_str.join(", ")
                    ));
                }
                InstData::ParamRef { index, name } => {
                    let name_str = self.interner.get(*name);
                    out.push_str(&format!("param {} ({})\n", index, name_str));
                }
                InstData::Block { extra_start, len } => {
                    out.push_str(&format!("block({}, {})\n", extra_start, len));
                }
                InstData::Alloc {
                    name,
                    is_mut,
                    ty,
                    init,
                } => {
                    let name_str = name
                        .map(|n| self.interner.get(n).to_string())
                        .unwrap_or_else(|| "_".to_string());
                    let mut_str = if *is_mut { "mut " } else { "" };
                    let ty_str = ty
                        .map(|t| format!(": {}", self.interner.get(t)))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "alloc {}{}{}= {}\n",
                        mut_str, name_str, ty_str, init
                    ));
                }
                InstData::VarRef { name } => {
                    let name_str = self.interner.get(*name);
                    out.push_str(&format!("var_ref {}\n", name_str));
                }
                InstData::Assign { name, value } => {
                    let name_str = self.interner.get(*name);
                    out.push_str(&format!("assign {} = {}\n", name_str, value));
                }
                InstData::StructDecl { name, fields } => {
                    let name_str = self.interner.get(*name);
                    let fields_str: Vec<String> = fields
                        .iter()
                        .map(|(fname, ftype)| {
                            format!(
                                "{}: {}",
                                self.interner.get(*fname),
                                self.interner.get(*ftype)
                            )
                        })
                        .collect();
                    out.push_str(&format!(
                        "struct {} {{ {} }}\n",
                        name_str,
                        fields_str.join(", ")
                    ));
                }
                InstData::StructInit { type_name, fields } => {
                    let type_str = self.interner.get(*type_name);
                    let fields_str: Vec<String> = fields
                        .iter()
                        .map(|(fname, value)| format!("{}: {}", self.interner.get(*fname), value))
                        .collect();
                    out.push_str(&format!(
                        "struct_init {} {{ {} }}\n",
                        type_str,
                        fields_str.join(", ")
                    ));
                }
                InstData::FieldGet { base, field } => {
                    let field_str = self.interner.get(*field);
                    out.push_str(&format!("field_get {}.{}\n", base, field_str));
                }
                InstData::FieldSet { base, field, value } => {
                    let field_str = self.interner.get(*field);
                    out.push_str(&format!("field_set {}.{} = {}\n", base, field_str, value));
                }
                InstData::EnumDecl { name, variants } => {
                    let name_str = self.interner.get(*name);
                    let variants_str: Vec<String> = variants
                        .iter()
                        .map(|v| self.interner.get(*v).to_string())
                        .collect();
                    out.push_str(&format!(
                        "enum {} {{ {} }}\n",
                        name_str,
                        variants_str.join(", ")
                    ));
                }
                InstData::EnumVariant { type_name, variant } => {
                    let type_str = self.interner.get(*type_name);
                    let variant_str = self.interner.get(*variant);
                    out.push_str(&format!("enum_variant {}::{}\n", type_str, variant_str));
                }
                InstData::ArrayInit { elements } => {
                    let elems_str: Vec<String> =
                        elements.iter().map(|e| format!("{}", e)).collect();
                    out.push_str(&format!("array_init [{}]\n", elems_str.join(", ")));
                }
                InstData::IndexGet { base, index } => {
                    out.push_str(&format!("index_get {}[{}]\n", base, index));
                }
                InstData::IndexSet { base, index, value } => {
                    out.push_str(&format!("index_set {}[{}] = {}\n", base, index, value));
                }
            }
        }
        out
    }
}

impl fmt::Display for RirPrinter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inst_ref_size() {
        assert_eq!(std::mem::size_of::<InstRef>(), 4);
    }

    #[test]
    fn test_add_and_get_inst() {
        let mut rir = Rir::new();
        let inst = Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        };
        let inst_ref = rir.add_inst(inst);

        let retrieved = rir.get(inst_ref);
        assert!(matches!(retrieved.data, InstData::IntConst(42)));
    }
}
