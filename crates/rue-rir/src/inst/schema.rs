//! Instruction and source-level schema types.

use super::*;

/// A reference to an instruction in the RIR.
///
/// This is a lightweight handle (4 bytes) that indexes into the instruction array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

impl PayloadFallback for InstRef {
    fn payload_fallback() -> Self {
        Self(u32::MAX)
    }
}

/// A directive in the RIR (e.g., @allow(unused_variable))
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RirDirective {
    /// Directive name (e.g., "allow")
    pub name: Spur,
    /// Arguments (e.g., ["unused_variable"])
    pub args: Vec<Spur>,
    /// Span covering the directive
    pub span: Span,
}

/// Parameter passing mode in RIR.
#[repr(u32)]
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

impl RirParamMode {
    /// Convert the serialized parameter mode from the RIR extra array.
    ///
    /// Invalid values indicate corrupted RIR or a producer/consumer mismatch.
    /// They must not silently recover as normal by-value parameters, because
    /// that changes ownership and aliasing semantics.
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => RirParamMode::Normal,
            1 => RirParamMode::Inout,
            2 => RirParamMode::Borrow,
            _ => panic!("invalid RirParamMode value: {}", v),
        }
    }

    /// Serialize this mode into the RIR extra array.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// A parameter in a function declaration.
#[derive(Debug, Clone, Copy)]
pub struct RirParam {
    /// Parameter name
    pub name: Spur,
    /// Parameter type
    pub ty: RirTypeSyntaxRef,
    /// Parameter passing mode
    pub mode: RirParamMode,
    /// Whether this parameter is evaluated at compile time (declared with
    /// the `comptime` modifier; carried separately from `mode`, which stays
    /// `Normal` for comptime parameters)
    pub is_comptime: bool,
    /// Span of the parameter's name, used to point diagnostics (e.g. the
    /// duplicate-parameter error, RUE-349) at the offending occurrence.
    pub span: Span,
}

/// Argument passing mode in RIR.
#[repr(u32)]
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

impl RirArgMode {
    /// Convert the serialized call-argument mode from the RIR extra array.
    ///
    /// Invalid values indicate corrupted RIR or a producer/consumer mismatch.
    /// They must not silently recover as normal by-value arguments, because
    /// that changes ownership and aliasing semantics.
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => RirArgMode::Normal,
            1 => RirArgMode::Inout,
            2 => RirArgMode::Borrow,
            _ => panic!("invalid RirArgMode value: {}", v),
        }
    }

    /// Serialize this mode into the RIR extra array.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// An argument in a function call.
#[derive(Debug, Clone, Copy)]
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
    /// Integer literal pattern. The magnitude and sign are kept separate so
    /// Sema can range-check the literal against the scrutinee type exactly
    /// like `let` bindings (E0800/E0801) instead of silently wrapping
    /// (RUE-74). `negative` means the source had a leading `-`.
    Int {
        /// Magnitude of the literal as written (e.g. 128 for `-128`).
        value: u64,
        /// True if the pattern was written with a leading minus sign.
        negative: bool,
        /// Span of the pattern (including the minus sign, if any).
        span: Span,
    },
    /// Boolean literal pattern
    Bool(bool, Span),
    /// Path pattern for enum variants (e.g., `Color::Red` or `module.Color::Red`)
    Path {
        /// Optional module reference for qualified paths (e.g., the `module` in `module.Color::Red`)
        module: Option<InstRef>,
        /// Optional inline type-constructor call head — the instruction that
        /// reduces to the enum type at comptime for `F(args).Variant(..)`
        /// (RUE-596, spec 4.14:23). When `Some`, the enum is
        /// the reduction of this head and `type_name` is only the constructor
        /// function's name; `None` for an ordinary `Enum.Variant` pattern.
        ctor_head: Option<InstRef>,
        /// The enum type name
        type_name: Spur,
        /// The variant name
        variant: Spur,
        /// Payload binding names for a tuple-variant pattern `Circle(r)`
        /// (RUE-221), in payload order. Empty for a discriminant-only pattern.
        bindings: Vec<Spur>,
        /// Span of the pattern
        span: Span,
    },
}

impl RirPattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            RirPattern::Wildcard(span) => *span,
            RirPattern::Int { span, .. } => *span,
            RirPattern::Bool(_, span) => *span,
            RirPattern::Path { span, .. } => *span,
        }
    }
}

/// One typed step in a definition-relative structural path. Indices are local
/// to the immediately enclosing syntax node, never absolute source positions
/// or global RIR instruction indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RirStructuralPathSegment {
    Body,
    ParameterType(u32),
    ReturnType,
    Statement(u32),
    Operand(u32),
    Branch(u32),
    MatchArm(u32),
    FieldType(u32),
    VariantPayload { variant: u32, payload: u32 },
    Method(u32),
    AnonymousType(u32),
    StringLiteral(u32),
    ReadOnlyData(u32),
}

/// Trivia- and absolute-position-insensitive identity of a structural source
/// site, relative to the semantic definition that owns it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RirStructuralAnchor(std::sync::Arc<[RirStructuralPathSegment]>);

impl RirStructuralAnchor {
    pub fn new(segments: impl Into<std::sync::Arc<[RirStructuralPathSegment]>>) -> Self {
        Self(segments.into())
    }

    pub fn segments(&self) -> &[RirStructuralPathSegment] {
        &self.0
    }
}

/// A source occurrence whose structural anchor is materialized only if semantic
/// resolution proves that the read names module-level read-only data.
///
/// AstGen shares the immutable prefix nodes between syntax descendants and
/// stores the occurrence's final segment inline. Extending the producer cursor
/// is O(1), and recording a variable read performs no allocation or path copy.
#[derive(Debug, Clone)]
pub(crate) struct RirDeferredStructuralAnchor {
    pub(crate) prefix: Option<std::sync::Arc<RirStructuralPathPrefix>>,
    pub(crate) tail: RirStructuralPathSegment,
    pub(crate) len: usize,
    pub(crate) flat: Option<RirStructuralAnchor>,
}

#[derive(Debug)]
pub(crate) struct RirStructuralPathPrefix {
    pub(crate) parent: Option<std::sync::Arc<RirStructuralPathPrefix>>,
    pub(crate) segment: RirStructuralPathSegment,
    pub(crate) len: usize,
}

pub(crate) const MAX_DEFERRED_STRUCTURAL_PATH: usize = rue_error::MAX_NESTING_DEPTH * 4;

impl RirDeferredStructuralAnchor {
    pub(crate) fn new(
        prefix: Option<std::sync::Arc<RirStructuralPathPrefix>>,
        tail: RirStructuralPathSegment,
    ) -> Self {
        let len = prefix.as_ref().map_or(1, |prefix| prefix.len + 1);
        Self {
            prefix,
            tail,
            len,
            flat: None,
        }
    }

    pub(crate) fn from_flat(anchor: RirStructuralAnchor) -> Self {
        let segments = anchor.segments();
        let tail = segments
            .last()
            .copied()
            .unwrap_or(RirStructuralPathSegment::Body);
        Self {
            prefix: None,
            tail,
            len: segments.len(),
            flat: Some(anchor),
        }
    }

    /// Materialize the original public flat anchor at the semantic const-use
    /// boundary. Parser-produced chains are bounded by `MAX_NESTING_DEPTH`;
    /// packed input is validated before reaching this representation.
    pub(crate) fn materialize(&self) -> RirStructuralAnchor {
        if let Some(flat) = &self.flat {
            return flat.clone();
        }
        let mut segments = vec![self.tail; self.len];
        let mut index = self.len - 1;
        let mut cursor = self.prefix.as_deref();
        while let Some(node) = cursor {
            index -= 1;
            segments[index] = node.segment;
            cursor = node.parent.as_deref();
        }
        RirStructuralAnchor::new(segments)
    }

    pub(crate) fn retained_allocation_charge(&self) -> u64 {
        if let Some(flat) = &self.flat {
            return std::mem::size_of_val(flat.segments()) as u64;
        }
        self.prefix.as_ref().map_or(0, |prefix| {
            prefix.len as u64 * std::mem::size_of::<RirStructuralPathPrefix>() as u64
        })
    }
}

impl PartialEq for RirDeferredStructuralAnchor {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len || self.tail != other.tail {
            return false;
        }
        if let (Some(left), Some(right)) = (&self.flat, &other.flat) {
            return left == right;
        }
        if let Some(flat) = &self.flat {
            return chained_prefix_equals(
                other.prefix.as_deref(),
                &flat.segments()[..self.len - 1],
            );
        }
        if let Some(flat) = &other.flat {
            return chained_prefix_equals(self.prefix.as_deref(), &flat.segments()[..self.len - 1]);
        }
        let mut left = self.prefix.as_deref();
        let mut right = other.prefix.as_deref();
        while let (Some(a), Some(b)) = (left, right) {
            if a.segment != b.segment {
                return false;
            }
            left = a.parent.as_deref();
            right = b.parent.as_deref();
        }
        left.is_none() && right.is_none()
    }
}

impl Eq for RirDeferredStructuralAnchor {}

fn chained_prefix_equals(
    mut node: Option<&RirStructuralPathPrefix>,
    segments: &[RirStructuralPathSegment],
) -> bool {
    let mut index = segments.len();
    while let Some(current) = node {
        if index == 0 || current.segment != segments[index - 1] {
            return false;
        }
        index -= 1;
        node = current.parent.as_deref();
    }
    index == 0
}

/// A single RIR instruction.
#[derive(Debug, PartialEq, Eq)]
pub struct Inst {
    pub data: InstData,
    pub span: Span,
}

/// The repeat count of an array-repeat literal `[value; count]` (RUE-235).
///
/// The count is a compile-time constant: either an integer literal (`[0; 128]`)
/// or a name referring to a `const` / `comptime` value parameter (`[0; N]`).
/// Named counts are resolved to a concrete value during semantic analysis using
/// the same const-eval machinery as array-type lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatCount {
    /// A literal count, e.g. the `128` in `[0; 128]`.
    Literal(u64),
    /// A named count, e.g. the `N` in `[0; N]`.
    Named(Spur),
}

/// A compiler-internal intrinsic synthesized directly into RIR.
///
/// These operations are not source intrinsics. Keeping their identity separate
/// from [`InstData::Intrinsic`] prevents a source spelling that resembles an
/// implementation detail from selecting compiler-only behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InternalIntrinsic {
    IterLen,
    CharScalar,
    CharNext,
    CharScalarLossy,
    CharNextLossy,
}

impl InternalIntrinsic {
    /// The diagnostic and RIR-printer spelling for this operation.
    ///
    /// This is presentation-only; compiler dispatch must match the enum.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IterLen => "__rue_iter_len",
            Self::CharScalar => "__rue_char_scalar",
            Self::CharNext => "__rue_char_next",
            Self::CharScalarLossy => "__rue_char_scalar_lossy",
            Self::CharNextLossy => "__rue_char_next_lossy",
        }
    }

    /// Number of RIR operands required by this operation.
    pub const fn arity(self) -> u32 {
        match self {
            Self::IterLen => 1,
            Self::CharScalar | Self::CharNext | Self::CharScalarLossy | Self::CharNextLossy => 2,
        }
    }
}

/// Instruction data - the actual operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstData {
    /// Integer constant
    IntConst(u64),

    /// Floating-point constant, held as the interned literal *text* with `_`
    /// separators removed (`1.5`, `1e9`, `6.022e23`).
    ///
    /// This node is deliberately untyped and undecoded (ADR-0065 §3,
    /// RUE-1069). A float literal is a `comptime_float`: an arbitrary-precision
    /// abstract constant that only becomes `f32` or `f64` when context demands
    /// one, with round-to-nearest applied at that point and an out-of-range
    /// magnitude reported as a compile error. Storing an `f64` here would
    /// perform that rounding before the target width — and therefore the
    /// range check — was known, so the digits travel intact and the phase that
    /// resolves the type parses them.
    FloatConst { text: Spur },

    /// Boolean constant
    BoolConst(bool),

    /// String constant with its definition-relative structural source site.
    StringConst {
        content: Spur,
        anchor: RirStructuralAnchor,
    },

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

    /// Try/`?` propagation: `operand?` unwraps an `Option`, evaluating to the
    /// `Some` payload, and early-returns `None` from the enclosing function
    /// when the operand is `None` (RUE-6, ADR-0038). Sema lowers it to a
    /// discriminant match with an early `return` on the `None` arm; the
    /// enclosing function must itself return an `Option`.
    Try { operand: InstRef },

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
    ///
    /// `iter_borrow` names the collection variable held as a scoped shared
    /// borrow for the loop's duration when this loop is the desugaring of a
    /// `for` over a named variable (spec 4.8:26, RUE-233). Sema rejects
    /// mutation of that variable inside the body (E0428). `None` for a plain
    /// `loop {}` or a `for` over a temporary (which is unnameable, so
    /// unmutatable, in the body).
    InfiniteLoop {
        body: InstRef,
        iter_borrow: Option<Spur>,
    },

    /// Match expression: match scrutinee { pattern => expr, ... }
    /// Arms are stored in the extra array using add_match_arms/get_match_arms.
    Match {
        /// The value being matched
        scrutinee: InstRef,
        /// Index into extra data where arms start
        arms: RirMatchArmsRange,
    },

    /// Break: exits the innermost loop.
    /// `value` is a value operand (e.g. `break 42`); it is carried through
    /// for diagnostics but always rejected by sema - break does not carry a
    /// value (see spec 4.8).
    Break { value: Option<InstRef> },

    /// Continue: jumps to the next iteration of the innermost loop
    Continue,

    /// Function definition
    /// Contains: name symbol, parameters, return type symbol, body instruction ref
    /// Directives and params are stored in the extra array.
    FnDecl {
        /// Index into extra data where directives start
        directives: RirDirectivesRange,
        /// Whether this function is public (requires --preview modules)
        is_pub: bool,
        /// Whether this function is marked `unchecked` (can only be called from checked blocks)
        is_unchecked: bool,
        /// Whether this is a foreign `extern "C"` declaration (ADR-0064 C FFI):
        /// a body-less import lowered to an undefined linker symbol. When true
        /// `body` is a synthesized placeholder that is never analyzed or
        /// code-generated.
        is_extern: bool,
        /// Whether this is a Rue-to-C export (`pub extern "C" fn`, ADR-0064 P4):
        /// an ordinary Rue function body that is *also* exposed to C callers
        /// under its unmangled source name via a C-ABI callee thunk. Unlike
        /// `is_extern`, an export keeps a real body, is analyzed, gets a CFG,
        /// and is code-generated; the flag only marks that a C entry thunk must
        /// be emitted and that the signature is validated for the C boundary.
        is_c_export: bool,
        name: Spur,
        /// Index into extra data where params start
        params: RirParamsRange,
        return_type: RirTypeSyntaxRef,
        body: InstRef,
        /// Whether this function/method takes `self` as a receiver.
        /// Only true for methods in impl blocks that have a self parameter.
        /// Used by sema to know to add the implicit self parameter.
        has_self: bool,
        /// The receiver's passing mode when `has_self` is true (`Normal`
        /// by-value, `Borrow`, or `Inout`; RUE-15). Always `Normal` for
        /// associated functions and free functions.
        self_mode: RirParamMode,
        /// Whether the receiver is declared `mut self`: a by-value receiver
        /// that binds mutably in the method body. Body-local only — it does
        /// not affect the method's signature, call sites, or structural
        /// identity, and is always false unless `has_self` is true with
        /// `self_mode == Normal`.
        self_is_mut: bool,
        /// Whether the result position is `-> borrow T` (ADR-0062): the
        /// declaration is a place-returning accessor whose body yields a
        /// second-class borrow of a receiver projection. `return_type` holds
        /// the borrowed element type `T`.
        returns_borrow: bool,
        /// Whether the result position is `-> inout T` (ADR-0062 phase 2).
        returns_inout: bool,
    },

    /// Constant declaration
    /// Contains: name symbol, optional type, initializer expression ref
    /// Directives are stored in the extra array.
    /// Used for module re-exports: `pub const strings = @import("utils/strings.rue");`
    ConstDecl {
        /// Index into extra data where directives start
        directives: RirDirectivesRange,
        /// Whether this constant is public (requires --preview modules)
        is_pub: bool,
        /// Constant name
        name: Spur,
        /// Optional structured type annotation (`None` when inferred).
        ty: Option<RirTypeSyntaxRef>,
        /// Initializer expression
        init: InstRef,
    },

    /// Function call
    /// Args are stored in the extra array using add_call_args/get_call_args.
    Call {
        /// Function name
        name: Spur,
        /// Index into extra data where args start
        args: RirCallArgsRange,
    },

    /// Intrinsic call with expression arguments (e.g., @dbg)
    /// Args are stored in the typed intrinsic-argument payload family.
    Intrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Index into extra data where args start
        args: RirIntrinsicArgsRange,
    },

    /// Compiler-internal intrinsic with expression arguments.
    ///
    /// Args are stored in the typed internal-intrinsic payload family.
    InternalIntrinsic {
        intrinsic: InternalIntrinsic,
        args: RirInternalIntrinsicArgsRange,
    },

    /// Intrinsic call with a type argument (e.g., @size_of, @align_of)
    TypeIntrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Structured type argument.
        type_arg: RirTypeSyntaxRef,
    },

    /// `@offset_of(T, field)` — the compile-time byte offset of `field` within
    /// struct type `T` (RUE-301). Carries a type argument (as an interned
    /// string, exactly like [`InstData::TypeIntrinsic`]) and the field name;
    /// neither is an `InstRef`, so this variant has no operands to renumber.
    OffsetOf {
        /// Structured type argument.
        type_arg: RirTypeSyntaxRef,
        /// Field name whose offset is requested.
        field: Spur,
    },

    /// Return value from function (None for `return;` in unit-returning functions)
    Ret(Option<InstRef>),

    /// Yield a place from a `-> borrow T` accessor body (ADR-0062). The
    /// operand is the place expression the accessor hands out; valid only as
    /// the trailing exit of an accessor body (enforced in sema).
    Yield(InstRef),

    /// Block of instructions (for function bodies)
    /// The result is the last instruction in the block
    Block {
        /// Index into extra data where instruction refs start
        instructions: RirBlockInstsRange,
    },

    // Variable operations
    /// Local variable declaration: allocates storage and initializes
    /// If name is None, this is a wildcard pattern that discards the value
    /// Directives are stored in the extra array using add_directives/get_directives.
    Alloc {
        /// Index into extra data where directives start
        directives: RirDirectivesRange,
        /// Variable name (None for wildcard `_` pattern that discards the value)
        name: Option<Spur>,
        /// Whether the variable is mutable
        is_mut: bool,
        /// Optional type annotation
        ty: Option<RirTypeSyntaxRef>,
        /// Initial value instruction
        init: InstRef,
        /// True when this binding is a `for`-loop element binding, which reads
        /// the element by shared borrow (spec 4.8:26): the initializer is
        /// analyzed as a by-ref read (never a move-out of the collection, so a
        /// non-Copy element is fine, RUE-259) and a non-Copy binder is a
        /// non-owning borrow slot that drop elaboration must not drop (the
        /// collection retains ownership and drops the element).
        iter_elem: bool,
    },

    /// Variable reference: reads the value of a variable
    VarRef {
        /// Variable name
        name: Spur,
        /// Stable source occurrence for named read-only data materialized at
        /// this reference. Synthesized references do not carry one.
        anchor: Option<RirStructuralAnchor>,
    },

    /// Assignment: stores a value into a mutable variable
    Assign {
        /// Variable name
        name: Spur,
        /// Value to store
        value: InstRef,
    },

    /// Assignment to an expression that produces a place (currently a
    /// mutable accessor result). Kept distinct from field/index assignment so
    /// source field names cannot collide with compiler-generated syntax.
    PlaceSet { place: InstRef, value: InstRef },

    // Struct operations
    /// Struct type declaration
    /// Directives, fields, and methods are stored in the extra array.
    StructDecl {
        /// Index into extra data where directives start
        directives: RirDirectivesRange,
        /// Whether this struct is public (requires --preview modules)
        is_pub: bool,
        /// Whether this struct is a linear type (must be consumed)
        is_linear: bool,
        /// Struct name
        name: Spur,
        /// Index into extra data where fields start
        fields: RirStructFieldsRange,
        /// Index into extra data where method refs start
        methods: RirStructMethodsRange,
    },

    /// Struct literal: creates a new struct instance
    /// Fields are stored in the extra array using add_field_inits/get_field_inits.
    StructInit {
        /// Optional module reference (for qualified struct literals like `module.Point { ... }`)
        /// If Some, the struct is looked up in the module's exports.
        module: Option<InstRef>,
        /// Optional inline type-constructor call head — the instruction that
        /// reduces to the struct type at comptime for `F(args) { ... }` (RUE-596,
        /// spec 4.14:23). When `Some`, the struct type is
        /// the reduction of this head and `type_name` is only the constructor
        /// function's name (kept for diagnostics); `None` for `Name { ... }`.
        ctor_head: Option<InstRef>,
        /// Struct type name
        type_name: Spur,
        /// Index into extra data where fields start
        fields: RirFieldInitsRange,
        /// Span of the first field-init-shorthand field, if any (`P { x }`
        /// desugaring to `P { x: x }`, RUE-613, stabilized in RUE-628). `Some`
        /// iff at least one field used the shorthand; retained as diagnostic
        /// provenance for the shorthand form. `None` when every field was
        /// written explicitly (`P { x: x }`).
        shorthand_span: Option<Span>,
    },

    /// Field access: reads a field from a struct
    FieldGet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Spur,
    },

    /// Field assignment: writes a value to a struct field
    FieldSet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Spur,
        /// Value to store
        value: InstRef,
    },

    // Enum operations
    /// Enum type declaration
    /// Variants are stored in the typed enum-variant payload family.
    EnumDecl {
        /// Whether this enum is public (requires --preview modules)
        is_pub: bool,
        /// Whether importing modules must include a wildcard when matching.
        is_non_exhaustive: bool,
        /// Enum name
        name: Spur,
        /// Index into extra data where variants start
        variants: RirEnumVariantsRange,
        /// Index into extra data where the tuple-variant payloads start
        /// (RUE-221). The region is a self-describing flat sequence: for each
        /// variant in declaration order, a count `k` followed by `k`
        /// type-name symbols (as `Spur`s). A count of 0 means a
        /// discriminant-only variant. `payloads_len` is the total number of
        /// u32 words in the region (0 when no variant carries a payload).
        payloads: RirEnumPayloadsRange,
    },

    /// Enum variant: creates a value of an enum type
    EnumVariant {
        /// Optional module reference (for qualified paths like `module.Color::Red`)
        /// If Some, the enum is looked up in the module's exports.
        module: Option<InstRef>,
        /// Enum type name
        type_name: Spur,
        /// Variant name
        variant: Spur,
    },

    // Array operations
    /// Array literal: creates a new array from element values
    /// Elements are stored in the typed array-element payload family.
    ArrayInit {
        /// Index into extra data where elements start
        elements: RirArrayElemsRange,
    },

    /// Array-repeat literal `[value; count]` (RUE-235): creates an array of
    /// `count` copies of a single `value`. `value` is evaluated once and its
    /// (Copy) result fills every slot. The count is a compile-time constant
    /// resolved during semantic analysis.
    ArrayRepeat {
        /// The value being repeated (evaluated once).
        value: InstRef,
        /// The repeat count (literal or named compile-time constant).
        count: RepeatCount,
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
    /// Method call: receiver.method(args)
    /// Args are stored in the extra array using add_call_args/get_call_args.
    MethodCall {
        /// Receiver expression (the struct value)
        receiver: InstRef,
        /// Method name
        method: Spur,
        /// Index into extra data where args start
        args: RirCallArgsRange,
    },

    /// User-defined destructor declaration: drop fn TypeName(self) { ... }
    DropFnDecl {
        /// The struct type this destructor is for
        type_name: Spur,
        /// Destructor body instruction ref
        body: InstRef,
    },

    /// Comptime block expression: comptime { expr }
    /// The inner expression must be evaluable at compile time.
    Comptime {
        /// The expression to evaluate at compile time
        expr: InstRef,
    },

    /// Checked block expression: checked { expr }
    /// Unchecked operations (raw pointer manipulation, calling unchecked functions)
    /// are only allowed inside checked blocks.
    Checked {
        /// The expression inside the checked block
        expr: InstRef,
    },

    /// Type constant: a type used as a value expression (e.g., `i32` in `identity(i32, 42)`)
    /// Carries the exact structured parser-owned syntax.
    TypeConst { type_name: RirTypeSyntaxRef },

    /// Anonymous struct type: a struct type used as a value expression
    /// (e.g., `struct { first: T, second: T, fn method(self) -> T { ... } }` in comptime type construction)
    /// Fields are stored in the extra array using add_field_decls/get_field_decls.
    /// Methods are stored as InstRefs to FnDecl instructions in the extra array.
    AnonStructType {
        /// Index into extra data where fields start
        fields: RirAnonStructFieldsRange,
        /// Index into extra data where method InstRefs start
        methods: RirAnonStructMethodsRange,
        /// Structural occurrence relative to the producing definition body.
        anchor: RirStructuralAnchor,
    },

    /// Anonymous enum type: an enum (sum) type used as a value expression
    /// (e.g., `enum { Some(T), None }` in comptime type construction). The
    /// enum analog of [`InstData::AnonStructType`]; enables generic sum types
    /// like `Option`/`Result` as comptime type functions (ADR-0038, RUE-6).
    /// Variant names and tuple-variant payloads are encoded exactly
    /// as in [`InstData::EnumDecl`].
    AnonEnumType {
        /// Index into extra data where variant name symbols start
        variants: RirAnonEnumVariantsRange,
        /// Index into extra data where the tuple-variant payloads start,
        /// encoded as in [`InstData::EnumDecl`]: a self-describing flat
        /// sequence of `count` + `count` type-name symbols per variant.
        payloads: RirAnonEnumPayloadsRange,
        /// Structural occurrence relative to the producing definition body.
        anchor: RirStructuralAnchor,
    },
}
