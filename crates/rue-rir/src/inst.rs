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

/// A directive in the RIR (e.g., @allow(unused_variable))
#[derive(Debug, Clone)]
pub struct RirDirective {
    /// Directive name (e.g., "allow")
    pub name: Symbol,
    /// Arguments (e.g., ["unused_variable"])
    pub args: Vec<Symbol>,
    /// Span covering the directive
    pub span: Span,
}

/// Parameter passing mode in RIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RirParamMode {
    /// Normal pass-by-value parameter
    #[default]
    Normal,
    /// Inout parameter - mutated in place and returned to caller
    Inout,
    /// Borrow parameter - immutable borrow without ownership transfer
    Borrow,
}

/// A parameter in a function declaration.
#[derive(Debug, Clone)]
pub struct RirParam {
    /// Parameter name
    pub name: Symbol,
    /// Parameter type
    pub ty: Symbol,
    /// Parameter passing mode
    pub mode: RirParamMode,
}

/// Argument passing mode in RIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RirArgMode {
    /// Normal pass-by-value argument
    #[default]
    Normal,
    /// Inout argument - mutated in place
    Inout,
    /// Borrow argument - immutable borrow
    Borrow,
}

/// An argument in a function call.
#[derive(Debug, Clone)]
pub struct RirCallArg {
    /// The argument expression
    pub value: InstRef,
    /// The passing mode for this argument
    pub mode: RirArgMode,
}

impl RirCallArg {
    /// Returns true if this argument is passed as inout.
    /// This is a convenience method for backwards compatibility.
    pub fn is_inout(&self) -> bool {
        self.mode == RirArgMode::Inout
    }

    /// Returns true if this argument is passed as borrow.
    pub fn is_borrow(&self) -> bool {
        self.mode == RirArgMode::Borrow
    }
}

/// A pattern in a match expression (RIR level - untyped).
#[derive(Debug, Clone)]
pub enum RirPattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard(Span),
    /// Integer literal pattern (can be positive or negative)
    Int(i64, Span),
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
        // Debug assertion for u32 overflow - catches pathological inputs during development
        debug_assert!(
            self.instructions.len() < u32::MAX as usize,
            "RIR instruction count overflow: {} instructions exceeds u32::MAX - 1",
            self.instructions.len()
        );

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
        // Debug assertions for u32 overflow - catches pathological inputs during development
        debug_assert!(
            self.extra.len() <= u32::MAX as usize,
            "RIR extra data overflow: {} entries exceeds u32::MAX",
            self.extra.len()
        );
        debug_assert!(
            self.extra.len().saturating_add(data.len()) <= u32::MAX as usize,
            "RIR extra data would overflow: {} + {} exceeds u32::MAX",
            self.extra.len(),
            data.len()
        );

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
        /// Directives applied to this function
        directives: Vec<RirDirective>,
        name: Symbol,
        /// Parameters with names, types, and modes
        params: Vec<RirParam>,
        return_type: Symbol,
        body: InstRef,
        /// Whether this function/method takes `self` as a receiver.
        /// Only true for methods in impl blocks that have a self parameter.
        /// Used by sema to know to add the implicit self parameter.
        has_self: bool,
    },

    /// Function call
    Call {
        /// Function name
        name: Symbol,
        /// Arguments with optional inout flags
        args: Vec<RirCallArg>,
    },

    /// Intrinsic call with expression arguments (e.g., @dbg)
    Intrinsic {
        /// Intrinsic name (without @)
        name: Symbol,
        /// Argument instruction refs
        args: Vec<InstRef>,
    },

    /// Intrinsic call with a type argument (e.g., @size_of, @align_of)
    TypeIntrinsic {
        /// Intrinsic name (without @)
        name: Symbol,
        /// Type argument (as an interned string, e.g., "i32", "Point", "[i32; 4]")
        type_arg: Symbol,
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
        /// Directives applied to this let binding
        directives: Vec<RirDirective>,
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
        /// Directives applied to the struct (e.g., @copy)
        directives: Vec<RirDirective>,
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

    // Method operations
    /// Impl block declaration
    ImplDecl {
        /// Type name this impl block is for
        type_name: Symbol,
        /// Methods defined in this impl block (references to FnDecl instructions)
        methods: Vec<InstRef>,
    },

    /// Method call: receiver.method(args)
    MethodCall {
        /// Receiver expression (the struct value)
        receiver: InstRef,
        /// Method name
        method: Symbol,
        /// Arguments with optional inout flags
        args: Vec<RirCallArg>,
    },

    /// Associated function call: Type::function(args)
    AssocFnCall {
        /// Type name (e.g., Point)
        type_name: Symbol,
        /// Function name (e.g., origin)
        function: Symbol,
        /// Arguments with optional inout flags
        args: Vec<RirCallArg>,
    },

    /// User-defined destructor declaration: drop fn TypeName(self) { ... }
    DropFnDecl {
        /// The struct type this destructor is for
        type_name: Symbol,
        /// Destructor body instruction ref
        body: InstRef,
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
                    directives: _,
                    name,
                    params,
                    return_type,
                    body,
                    has_self,
                } => {
                    let name_str = self.interner.get(*name);
                    let ret_str = self.interner.get(*return_type);
                    let self_str = if *has_self { "self, " } else { "" };
                    let params_str: Vec<String> = params
                        .iter()
                        .map(|p| {
                            let mode_prefix = match p.mode {
                                RirParamMode::Inout => "inout ",
                                RirParamMode::Borrow => "borrow ",
                                RirParamMode::Normal => "",
                            };
                            format!(
                                "{}{}: {}",
                                mode_prefix,
                                self.interner.get(p.name),
                                self.interner.get(p.ty)
                            )
                        })
                        .collect();
                    out.push_str(&format!(
                        "fn {}({}{}) -> {} {{\n",
                        name_str,
                        self_str,
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
                    let args_str: Vec<String> = args
                        .iter()
                        .map(|a| match a.mode {
                            RirArgMode::Inout => format!("inout {}", a.value),
                            RirArgMode::Borrow => format!("borrow {}", a.value),
                            RirArgMode::Normal => format!("{}", a.value),
                        })
                        .collect();
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
                InstData::TypeIntrinsic { name, type_arg } => {
                    let name_str = self.interner.get(*name);
                    let type_str = self.interner.get(*type_arg);
                    out.push_str(&format!("type_intrinsic @{}({})\n", name_str, type_str));
                }
                InstData::ParamRef { index, name } => {
                    let name_str = self.interner.get(*name);
                    out.push_str(&format!("param {} ({})\n", index, name_str));
                }
                InstData::Block { extra_start, len } => {
                    out.push_str(&format!("block({}, {})\n", extra_start, len));
                }
                InstData::Alloc {
                    directives: _,
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
                InstData::StructDecl {
                    directives,
                    name,
                    fields,
                } => {
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
                    // Format directives
                    let directives_str = if directives.is_empty() {
                        String::new()
                    } else {
                        let dir_names: Vec<String> = directives
                            .iter()
                            .map(|d| format!("@{}", self.interner.get(d.name)))
                            .collect();
                        format!("{} ", dir_names.join(" "))
                    };
                    out.push_str(&format!(
                        "{}struct {} {{ {} }}\n",
                        directives_str,
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
                InstData::ImplDecl { type_name, methods } => {
                    let type_str = self.interner.get(*type_name);
                    let methods_str: Vec<String> =
                        methods.iter().map(|m| format!("{}", m)).collect();
                    out.push_str(&format!(
                        "impl {} {{ {} }}\n",
                        type_str,
                        methods_str.join(", ")
                    ));
                }
                InstData::MethodCall {
                    receiver,
                    method,
                    args,
                } => {
                    let method_str = self.interner.get(*method);
                    let args_str: Vec<String> = args
                        .iter()
                        .map(|a| match a.mode {
                            RirArgMode::Inout => format!("inout {}", a.value),
                            RirArgMode::Borrow => format!("borrow {}", a.value),
                            RirArgMode::Normal => format!("{}", a.value),
                        })
                        .collect();
                    out.push_str(&format!(
                        "method_call {}.{}({})\n",
                        receiver,
                        method_str,
                        args_str.join(", ")
                    ));
                }
                InstData::AssocFnCall {
                    type_name,
                    function,
                    args,
                } => {
                    let type_str = self.interner.get(*type_name);
                    let func_str = self.interner.get(*function);
                    let args_str: Vec<String> = args
                        .iter()
                        .map(|a| match a.mode {
                            RirArgMode::Inout => format!("inout {}", a.value),
                            RirArgMode::Borrow => format!("borrow {}", a.value),
                            RirArgMode::Normal => format!("{}", a.value),
                        })
                        .collect();
                    out.push_str(&format!(
                        "assoc_fn_call {}::{}({})\n",
                        type_str,
                        func_str,
                        args_str.join(", ")
                    ));
                }
                InstData::DropFnDecl { type_name, body } => {
                    let type_str = self.interner.get(*type_name);
                    out.push_str(&format!("drop fn {}(self) {{\n", type_str));
                    out.push_str(&format!("    {}\n", body));
                    out.push_str("}\n");
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
    use rue_intern::Interner;

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

    #[test]
    fn test_rir_is_empty() {
        let rir = Rir::new();
        assert!(rir.is_empty());
        assert_eq!(rir.len(), 0);
    }

    #[test]
    fn test_rir_extra_data() {
        let mut rir = Rir::new();
        let data = [1, 2, 3, 4, 5];
        let start = rir.add_extra(&data);
        assert_eq!(start, 0);

        let retrieved = rir.get_extra(start, 5);
        assert_eq!(retrieved, &data);

        // Add more extra data
        let data2 = [10, 20];
        let start2 = rir.add_extra(&data2);
        assert_eq!(start2, 5);
    }

    #[test]
    fn test_rir_iter() {
        let mut rir = Rir::new();
        rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(2, 3),
        });

        let items: Vec<_> = rir.iter().collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0.as_u32(), 0);
        assert_eq!(items[1].0.as_u32(), 1);
    }

    #[test]
    fn test_inst_ref_display() {
        let inst_ref = InstRef::from_raw(42);
        assert_eq!(format!("{}", inst_ref), "%42");
    }

    // RirPattern tests
    #[test]
    fn test_rir_pattern_wildcard_span() {
        let span = Span::new(10, 11);
        let pattern = RirPattern::Wildcard(span);
        assert_eq!(pattern.span(), span);
    }

    #[test]
    fn test_rir_pattern_int_span() {
        let span = Span::new(20, 22);
        let pattern = RirPattern::Int(42, span);
        assert_eq!(pattern.span(), span);

        // Test negative int
        let pattern_neg = RirPattern::Int(-100, span);
        assert_eq!(pattern_neg.span(), span);
    }

    #[test]
    fn test_rir_pattern_bool_span() {
        let span = Span::new(30, 34);
        let pattern = RirPattern::Bool(true, span);
        assert_eq!(pattern.span(), span);

        let pattern_false = RirPattern::Bool(false, span);
        assert_eq!(pattern_false.span(), span);
    }

    #[test]
    fn test_rir_pattern_path_span() {
        let span = Span::new(40, 50);
        let mut interner = Interner::new();
        let type_name = interner.intern("Color");
        let variant = interner.intern("Red");

        let pattern = RirPattern::Path {
            type_name,
            variant,
            span,
        };
        assert_eq!(pattern.span(), span);
    }

    // RirCallArg tests
    #[test]
    fn test_rir_call_arg_is_inout() {
        let arg_normal = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::Normal,
        };
        assert!(!arg_normal.is_inout());
        assert!(!arg_normal.is_borrow());

        let arg_inout = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::Inout,
        };
        assert!(arg_inout.is_inout());
        assert!(!arg_inout.is_borrow());

        let arg_borrow = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::Borrow,
        };
        assert!(!arg_borrow.is_inout());
        assert!(arg_borrow.is_borrow());
    }

    // RirPrinter tests
    fn create_printer_test_rir() -> (Rir, Interner) {
        let mut rir = Rir::new();
        let interner = Interner::new();
        (rir, interner)
    }

    #[test]
    fn test_printer_int_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const 42"));
    }

    #[test]
    fn test_printer_bool_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        rir.add_inst(Inst {
            data: InstData::BoolConst(false),
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const true"));
        assert!(output.contains("%1 = const false"));
    }

    #[test]
    fn test_printer_string_const() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let hello = interner.intern("hello world");
        rir.add_inst(Inst {
            data: InstData::StringConst(hello),
            span: Span::new(0, 13),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const \"hello world\""));
    }

    #[test]
    fn test_printer_unit_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const ()"));
    }

    #[test]
    fn test_printer_binary_ops() {
        let (mut rir, interner) = create_printer_test_rir();
        let lhs = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(2, 3),
        });

        // Test all binary operations
        let ops = vec![
            (InstData::Add { lhs, rhs }, "add"),
            (InstData::Sub { lhs, rhs }, "sub"),
            (InstData::Mul { lhs, rhs }, "mul"),
            (InstData::Div { lhs, rhs }, "div"),
            (InstData::Mod { lhs, rhs }, "mod"),
            (InstData::Eq { lhs, rhs }, "eq"),
            (InstData::Ne { lhs, rhs }, "ne"),
            (InstData::Lt { lhs, rhs }, "lt"),
            (InstData::Gt { lhs, rhs }, "gt"),
            (InstData::Le { lhs, rhs }, "le"),
            (InstData::Ge { lhs, rhs }, "ge"),
            (InstData::And { lhs, rhs }, "and"),
            (InstData::Or { lhs, rhs }, "or"),
            (InstData::BitAnd { lhs, rhs }, "bit_and"),
            (InstData::BitOr { lhs, rhs }, "bit_or"),
            (InstData::BitXor { lhs, rhs }, "bit_xor"),
            (InstData::Shl { lhs, rhs }, "shl"),
            (InstData::Shr { lhs, rhs }, "shr"),
        ];

        for (data, op_name) in ops {
            let mut test_rir = Rir::new();
            let lhs = test_rir.add_inst(Inst {
                data: InstData::IntConst(1),
                span: Span::new(0, 1),
            });
            let rhs = test_rir.add_inst(Inst {
                data: InstData::IntConst(2),
                span: Span::new(2, 3),
            });
            // Recreate the data with new refs
            let data = match op_name {
                "add" => InstData::Add { lhs, rhs },
                "sub" => InstData::Sub { lhs, rhs },
                "mul" => InstData::Mul { lhs, rhs },
                "div" => InstData::Div { lhs, rhs },
                "mod" => InstData::Mod { lhs, rhs },
                "eq" => InstData::Eq { lhs, rhs },
                "ne" => InstData::Ne { lhs, rhs },
                "lt" => InstData::Lt { lhs, rhs },
                "gt" => InstData::Gt { lhs, rhs },
                "le" => InstData::Le { lhs, rhs },
                "ge" => InstData::Ge { lhs, rhs },
                "and" => InstData::And { lhs, rhs },
                "or" => InstData::Or { lhs, rhs },
                "bit_and" => InstData::BitAnd { lhs, rhs },
                "bit_or" => InstData::BitOr { lhs, rhs },
                "bit_xor" => InstData::BitXor { lhs, rhs },
                "shl" => InstData::Shl { lhs, rhs },
                "shr" => InstData::Shr { lhs, rhs },
                _ => unreachable!(),
            };
            test_rir.add_inst(Inst {
                data,
                span: Span::new(0, 5),
            });

            let printer = RirPrinter::new(&test_rir, &interner);
            let output = printer.to_string();
            let expected = format!("%2 = {} %0, %1", op_name);
            assert!(
                output.contains(&expected),
                "Expected '{}' in output:\n{}",
                expected,
                output
            );
        }
    }

    #[test]
    fn test_printer_unary_ops() {
        let (mut rir, interner) = create_printer_test_rir();
        let operand = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        rir.add_inst(Inst {
            data: InstData::Neg { operand },
            span: Span::new(0, 3),
        });
        rir.add_inst(Inst {
            data: InstData::Not { operand },
            span: Span::new(0, 3),
        });
        rir.add_inst(Inst {
            data: InstData::BitNot { operand },
            span: Span::new(0, 3),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("neg %0"));
        assert!(output.contains("not %0"));
        assert!(output.contains("bit_not %0"));
    }

    #[test]
    fn test_printer_branch() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let then_block = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let else_block = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        // With else block
        rir.add_inst(Inst {
            data: InstData::Branch {
                cond,
                then_block,
                else_block: Some(else_block),
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("branch %0, %1, %2"));
    }

    #[test]
    fn test_printer_branch_no_else() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let then_block = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Branch {
                cond,
                then_block,
                else_block: None,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        // Should not have the third argument
        assert!(output.contains("branch %0, %1\n"));
    }

    #[test]
    fn test_printer_loop() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Loop { cond, body },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("loop %0, %1"));
    }

    #[test]
    fn test_printer_infinite_loop() {
        let (mut rir, interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::InfiniteLoop { body },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("infinite_loop %0"));
    }

    #[test]
    fn test_printer_break_continue() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::Break,
            span: Span::new(0, 5),
        });
        rir.add_inst(Inst {
            data: InstData::Continue,
            span: Span::new(0, 8),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("break\n"));
        assert!(output.contains("continue\n"));
    }

    #[test]
    fn test_printer_ret() {
        let (mut rir, interner) = create_printer_test_rir();
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        // Return with value
        rir.add_inst(Inst {
            data: InstData::Ret(Some(value)),
            span: Span::new(0, 10),
        });
        // Return without value
        rir.add_inst(Inst {
            data: InstData::Ret(None),
            span: Span::new(0, 6),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("ret %0"));
        assert!(output.contains("%2 = ret\n"));
    }

    #[test]
    fn test_printer_fn_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.intern("main");
        let return_type = interner.intern("i32");
        let param_name = interner.intern("x");
        let param_type = interner.intern("i32");

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives: vec![],
                name,
                params: vec![RirParam {
                    name: param_name,
                    ty: param_type,
                    mode: RirParamMode::Normal,
                }],
                return_type,
                body,
                has_self: false,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("fn main(x: i32) -> i32"));
    }

    #[test]
    fn test_printer_fn_decl_with_self() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let name = interner.intern("get_x");
        let return_type = interner.intern("i32");

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives: vec![],
                name,
                params: vec![],
                return_type,
                body,
                has_self: true,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("fn get_x(self, ) -> i32"));
    }

    #[test]
    fn test_printer_fn_decl_param_modes() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });

        let name = interner.intern("modify");
        let return_type = interner.intern("()");
        let param1_name = interner.intern("a");
        let param1_type = interner.intern("i32");
        let param2_name = interner.intern("b");
        let param2_type = interner.intern("i32");
        let param3_name = interner.intern("c");
        let param3_type = interner.intern("i32");

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives: vec![],
                name,
                params: vec![
                    RirParam {
                        name: param1_name,
                        ty: param1_type,
                        mode: RirParamMode::Normal,
                    },
                    RirParam {
                        name: param2_name,
                        ty: param2_type,
                        mode: RirParamMode::Inout,
                    },
                    RirParam {
                        name: param3_name,
                        ty: param3_type,
                        mode: RirParamMode::Borrow,
                    },
                ],
                return_type,
                body,
                has_self: false,
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("a: i32"));
        assert!(output.contains("inout b: i32"));
        assert!(output.contains("borrow c: i32"));
    }

    #[test]
    fn test_printer_call() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let name = interner.intern("foo");

        rir.add_inst(Inst {
            data: InstData::Call {
                name,
                args: vec![RirCallArg {
                    value: arg,
                    mode: RirArgMode::Normal,
                }],
            },
            span: Span::new(0, 8),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("call foo(%0)"));
    }

    #[test]
    fn test_printer_call_with_arg_modes() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let arg3 = rir.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 1),
        });

        let name = interner.intern("modify");

        rir.add_inst(Inst {
            data: InstData::Call {
                name,
                args: vec![
                    RirCallArg {
                        value: arg1,
                        mode: RirArgMode::Normal,
                    },
                    RirCallArg {
                        value: arg2,
                        mode: RirArgMode::Inout,
                    },
                    RirCallArg {
                        value: arg3,
                        mode: RirArgMode::Borrow,
                    },
                ],
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("call modify(%0, inout %1, borrow %2)"));
    }

    #[test]
    fn test_printer_intrinsic() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.intern("dbg");

        rir.add_inst(Inst {
            data: InstData::Intrinsic {
                name,
                args: vec![arg],
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("intrinsic @dbg(%0)"));
    }

    #[test]
    fn test_printer_type_intrinsic() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.intern("size_of");
        let type_arg = interner.intern("i32");

        rir.add_inst(Inst {
            data: InstData::TypeIntrinsic { name, type_arg },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("type_intrinsic @size_of(i32)"));
    }

    #[test]
    fn test_printer_param_ref() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.intern("x");

        rir.add_inst(Inst {
            data: InstData::ParamRef { index: 0, name },
            span: Span::new(0, 1),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("param 0 (x)"));
    }

    #[test]
    fn test_printer_block() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::Block {
                extra_start: 0,
                len: 3,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("block(0, 3)"));
    }

    #[test]
    fn test_printer_alloc() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.intern("x");
        let ty = interner.intern("i32");

        // Normal alloc with type
        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives: vec![],
                name: Some(name),
                is_mut: false,
                ty: Some(ty),
                init,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc x: i32= %0"));
    }

    #[test]
    fn test_printer_alloc_mut() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.intern("x");

        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives: vec![],
                name: Some(name),
                is_mut: true,
                ty: None,
                init,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc mut x= %0"));
    }

    #[test]
    fn test_printer_alloc_wildcard() {
        let (mut rir, interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives: vec![],
                name: None,
                is_mut: false,
                ty: None,
                init,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc _= %0"));
    }

    #[test]
    fn test_printer_var_ref() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.intern("x");

        rir.add_inst(Inst {
            data: InstData::VarRef { name },
            span: Span::new(0, 1),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("var_ref x"));
    }

    #[test]
    fn test_printer_assign() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let name = interner.intern("x");

        rir.add_inst(Inst {
            data: InstData::Assign { name, value },
            span: Span::new(0, 6),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assign x = %0"));
    }

    #[test]
    fn test_printer_struct_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.intern("Point");
        let x_name = interner.intern("x");
        let y_name = interner.intern("y");
        let i32_type = interner.intern("i32");

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives: vec![],
                name,
                fields: vec![(x_name, i32_type), (y_name, i32_type)],
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct Point { x: i32, y: i32 }"));
    }

    #[test]
    fn test_printer_struct_decl_with_directive() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.intern("Point");
        let x_name = interner.intern("x");
        let i32_type = interner.intern("i32");
        let copy_name = interner.intern("copy");

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives: vec![RirDirective {
                    name: copy_name,
                    args: vec![],
                    span: Span::new(0, 5),
                }],
                name,
                fields: vec![(x_name, i32_type)],
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("@copy struct Point { x: i32 }"));
    }

    #[test]
    fn test_printer_struct_init() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let x_val = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });
        let y_val = rir.add_inst(Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 2),
        });

        let type_name = interner.intern("Point");
        let x_name = interner.intern("x");
        let y_name = interner.intern("y");

        rir.add_inst(Inst {
            data: InstData::StructInit {
                type_name,
                fields: vec![(x_name, x_val), (y_name, y_val)],
            },
            span: Span::new(0, 25),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct_init Point { x: %0, y: %1 }"));
    }

    #[test]
    fn test_printer_field_get() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for a struct value
            span: Span::new(0, 1),
        });

        let field = interner.intern("x");

        rir.add_inst(Inst {
            data: InstData::FieldGet { base, field },
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("field_get %0.x"));
    }

    #[test]
    fn test_printer_field_set() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder
            span: Span::new(0, 1),
        });
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let field = interner.intern("x");

        rir.add_inst(Inst {
            data: InstData::FieldSet { base, field, value },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("field_set %0.x = %1"));
    }

    #[test]
    fn test_printer_enum_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let name = interner.intern("Color");
        let red = interner.intern("Red");
        let green = interner.intern("Green");
        let blue = interner.intern("Blue");

        rir.add_inst(Inst {
            data: InstData::EnumDecl {
                name,
                variants: vec![red, green, blue],
            },
            span: Span::new(0, 35),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("enum Color { Red, Green, Blue }"));
    }

    #[test]
    fn test_printer_enum_variant() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let type_name = interner.intern("Color");
        let variant = interner.intern("Red");

        rir.add_inst(Inst {
            data: InstData::EnumVariant { type_name, variant },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("enum_variant Color::Red"));
    }

    #[test]
    fn test_printer_array_init() {
        let (mut rir, interner) = create_printer_test_rir();
        let elem1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let elem2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let elem3 = rir.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::ArrayInit {
                elements: vec![elem1, elem2, elem3],
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("array_init [%0, %1, %2]"));
    }

    #[test]
    fn test_printer_index_get() {
        let (mut rir, interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for array
            span: Span::new(0, 1),
        });
        let index = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::IndexGet { base, index },
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("index_get %0[%1]"));
    }

    #[test]
    fn test_printer_index_set() {
        let (mut rir, interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for array
            span: Span::new(0, 1),
        });
        let index = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        rir.add_inst(Inst {
            data: InstData::IndexSet { base, index, value },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("index_set %0[%1] = %2"));
    }

    // Impl block tests
    #[test]
    fn test_printer_impl_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();

        // Create a method first
        let method_body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        let method_name = interner.intern("get_x");
        let return_type = interner.intern("i32");

        let method_ref = rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives: vec![],
                name: method_name,
                params: vec![],
                return_type,
                body: method_body,
                has_self: true,
            },
            span: Span::new(0, 30),
        });

        let type_name = interner.intern("Point");

        rir.add_inst(Inst {
            data: InstData::ImplDecl {
                type_name,
                methods: vec![method_ref],
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("impl Point { %1 }"));
    }

    #[test]
    fn test_printer_method_call() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let receiver = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for struct value
            span: Span::new(0, 1),
        });
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let method = interner.intern("add");

        rir.add_inst(Inst {
            data: InstData::MethodCall {
                receiver,
                method,
                args: vec![RirCallArg {
                    value: arg,
                    mode: RirArgMode::Normal,
                }],
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("method_call %0.add(%1)"));
    }

    #[test]
    fn test_printer_method_call_with_arg_modes() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let receiver = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });

        let method = interner.intern("modify");

        rir.add_inst(Inst {
            data: InstData::MethodCall {
                receiver,
                method,
                args: vec![
                    RirCallArg {
                        value: arg1,
                        mode: RirArgMode::Inout,
                    },
                    RirCallArg {
                        value: arg2,
                        mode: RirArgMode::Borrow,
                    },
                ],
            },
            span: Span::new(0, 25),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("method_call %0.modify(inout %1, borrow %2)"));
    }

    #[test]
    fn test_printer_assoc_fn_call() {
        let (mut rir, mut interner) = create_printer_test_rir();

        let type_name = interner.intern("Point");
        let function = interner.intern("origin");

        rir.add_inst(Inst {
            data: InstData::AssocFnCall {
                type_name,
                function,
                args: vec![],
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assoc_fn_call Point::origin()"));
    }

    #[test]
    fn test_printer_assoc_fn_call_with_args() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 2),
        });

        let type_name = interner.intern("Point");
        let function = interner.intern("new");

        rir.add_inst(Inst {
            data: InstData::AssocFnCall {
                type_name,
                function,
                args: vec![
                    RirCallArg {
                        value: arg1,
                        mode: RirArgMode::Normal,
                    },
                    RirCallArg {
                        value: arg2,
                        mode: RirArgMode::Normal,
                    },
                ],
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assoc_fn_call Point::new(%0, %1)"));
    }

    #[test]
    fn test_printer_drop_fn_decl() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });

        let type_name = interner.intern("Resource");

        rir.add_inst(Inst {
            data: InstData::DropFnDecl { type_name, body },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("drop fn Resource(self)"));
    }

    // Match and pattern tests
    #[test]
    fn test_printer_match_wildcard() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms: vec![(RirPattern::Wildcard(Span::new(0, 1)), body)],
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { _ => %1 }"));
    }

    #[test]
    fn test_printer_match_int_pattern() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });
        let body1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let body_default = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms: vec![
                    (RirPattern::Int(1, Span::new(0, 1)), body1),
                    (RirPattern::Int(-5, Span::new(0, 2)), body2),
                    (RirPattern::Wildcard(Span::new(0, 1)), body_default),
                ],
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { 1 => %1, -5 => %2, _ => %3 }"));
    }

    #[test]
    fn test_printer_match_bool_pattern() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let body_true = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body_false = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms: vec![
                    (RirPattern::Bool(true, Span::new(0, 4)), body_true),
                    (RirPattern::Bool(false, Span::new(0, 5)), body_false),
                ],
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { true => %1, false => %2 }"));
    }

    #[test]
    fn test_printer_match_path_pattern() {
        let (mut rir, mut interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for enum value
            span: Span::new(0, 1),
        });
        let body_red = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body_green = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let body_default = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let color = interner.intern("Color");
        let red = interner.intern("Red");
        let green = interner.intern("Green");

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms: vec![
                    (
                        RirPattern::Path {
                            type_name: color,
                            variant: red,
                            span: Span::new(0, 10),
                        },
                        body_red,
                    ),
                    (
                        RirPattern::Path {
                            type_name: color,
                            variant: green,
                            span: Span::new(0, 12),
                        },
                        body_green,
                    ),
                    (RirPattern::Wildcard(Span::new(0, 1)), body_default),
                ],
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { Color::Red => %1, Color::Green => %2, _ => %3 }"));
    }

    #[test]
    fn test_printer_display_trait() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        // Test Display trait implementation
        let output = format!("{}", printer);
        assert!(output.contains("%0 = const 42"));
    }
}
