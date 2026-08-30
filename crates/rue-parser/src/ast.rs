//! Abstract Syntax Tree types for Rue.
//!
//! The AST represents the syntactic structure of the source code.
//! It closely mirrors the source syntax and preserves all information
//! needed for error reporting.
//!
//! ## SmallVec Usage
//!
//! Some non-recursive Vec fields use SmallVec to avoid heap allocation for
//! common small sizes:
//! - `Directives` (SmallVec<[Directive; 1]>) - most items have 0-1 directives
//!
//! ## Vec Usage (Cannot Use SmallVec)
//!
//! Vec fields containing recursive types (Expr) cannot use SmallVec because
//! Expr's size cannot be determined at compile time. These include:
//! - `Vec<CallArg>` - CallArg contains Expr
//! - `Vec<MatchArm>` - contains Expr
//! - `Vec<FieldInit>` - contains Box<Expr>
//! - `Vec<IntrinsicArg>` - contains Expr
//! - `Vec<Statement>` - Statement contains Expr
//! - `Vec<Expr>` - directly recursive
//!
//! The IR layers (RIR, AIR, CFG) use index-based references which avoid
//! this issue and are already efficiently allocated.

use std::fmt;

use lasso::{Key, Spur};
use rue_span::{FileId, Span};
use smallvec::SmallVec;

/// Type alias for a small vector of directives.
/// Most items have 0-1 directives, so we inline capacity for 1.
pub type Directives = SmallVec<[Directive; 1]>;

/// A complete source file (list of items).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ast {
    pub items: Vec<Item>,
}

impl Ast {
    /// Rebind every source span in this syntax tree to a snapshot-local file.
    ///
    /// Parsed syntax can be retained by content identity while a later source
    /// snapshot assigns that module a different [`FileId`]. Offsets remain
    /// valid because the source identity is unchanged; only the snapshot-local
    /// file component changes.
    pub fn rebind_file_id(&mut self, file_id: FileId) {
        for item in &mut self.items {
            rebind_item(item, file_id);
        }
    }
}

/// A directive that modifies compiler behavior for the following item or statement.
///
/// Directives use the `@name(args)` syntax and appear before items or statements.
/// For example: `@allow(unused_variable)`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    /// The directive name (without the @)
    pub name: Ident,
    /// Arguments to the directive
    pub args: Vec<DirectiveArg>,
    /// Span covering the entire directive
    pub span: Span,
}

/// An argument to a directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveArg {
    /// An identifier argument (e.g., `unused_variable` in `@allow(unused_variable)`)
    Ident(Ident),
}

/// A top-level item in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Function(Function),
    Struct(StructDecl),
    Enum(EnumDecl),
    DropFn(DropFn),
    /// A foreign-declaration block: `extern "C" { fn ...; }` (ADR-0064 C FFI).
    Extern(ExternBlock),
    /// Constant declaration (e.g., `const math = @import("math");`)
    Const(ConstDecl),
    /// Error node for recovered parse errors at item level.
    /// Used by error recovery to continue parsing after a syntax error.
    Error(Span),
}

/// A constant declaration.
///
/// Constants are compile-time values. In the context of the module system,
/// they're used for re-exports:
/// ```rue
/// // _utils.rue (directory module root)
/// pub const strings = @import("utils/strings.rue");
/// pub const helper = @import("utils/internal.rue").helper;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstDecl {
    /// Directives applied to this const
    pub directives: Directives,
    /// Visibility of this constant
    pub visibility: Visibility,
    /// Constant name
    pub name: Ident,
    /// Type annotation. Syntactically optional so the parser can accept both
    /// forms, but a *value* constant must carry one: an unannotated value
    /// constant is rejected as E0475 (spec 6.5:4). It is legitimately absent
    /// only for the constants that name no value type — module bindings,
    /// their aliases and re-exports (chapter 10), and callable function
    /// aliases (6.5:15).
    pub ty: Option<TypeExpr>,
    /// Initializer expression
    pub init: Box<Expr>,
    /// Span covering the entire const declaration
    pub span: Span,
    /// Whether the body contains a value-position anonymous `struct {..}` or
    /// `enum {..}` literal. Recorded by the parser, which has the only two
    /// production sites, so the definition index can skip the full
    /// `anonymous_type_sites` body walk for the overwhelming majority of
    /// declarations that contain none (RUE-1837).
    pub contains_anonymous_type_literal: bool,
}

/// A struct declaration.
///
/// Structs can contain both fields and methods. Methods are defined inline
/// within the struct block, not in separate impl blocks.
///
/// ```rue
/// struct Point {
///     x: i32,
///     y: i32,
///
///     fn distance(self) -> i32 {
///         self.x + self.y
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    /// Directives applied to this struct (e.g., @copy)
    pub directives: Directives,
    /// Visibility of this struct
    pub visibility: Visibility,
    /// Whether this struct is a linear type (must be consumed, cannot be dropped)
    pub is_linear: bool,
    /// Struct name
    pub name: Ident,
    /// Struct fields
    pub fields: Vec<FieldDecl>,
    /// Methods defined on this struct
    pub methods: Vec<Method>,
    /// Span covering the entire struct declaration
    pub span: Span,
}

/// A field declaration in a struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    /// Field name
    pub name: Ident,
    /// Field type
    pub ty: TypeExpr,
    /// Span covering the entire field declaration
    pub span: Span,
}

/// An enum declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDecl {
    /// Directives applied to this enum (currently `@non_exhaustive`).
    pub directives: Directives,
    /// Visibility of this enum
    pub visibility: Visibility,
    /// Enum name
    pub name: Ident,
    /// Enum variants
    pub variants: Vec<EnumVariant>,
    /// Span covering the entire enum declaration
    pub span: Span,
}

/// A variant in an enum declaration.
///
/// A variant may be discriminant-only (`Empty`) or a **tuple variant** that
/// carries positional payload data (`Circle(i32)`, `Rect(i32, i32)`). The
/// `payload` vector holds the payload field types in declaration order; it is
/// empty for a discriminant-only variant. Tuple variants require the
/// `enum_payloads` preview feature (RUE-221, ADR-0038).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    /// Variant name
    pub name: Ident,
    /// Payload field types (empty = discriminant-only variant).
    pub payload: Vec<TypeExpr>,
    /// Span covering the variant
    pub span: Span,
}

/// A user-defined destructor declaration.
///
/// Syntax: `drop fn TypeName(self) { body }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropFn {
    /// The struct type this destructor is for
    pub type_name: Ident,
    /// The self parameter
    pub self_param: SelfParam,
    /// Destructor body
    pub body: Expr,
    /// Span covering the entire drop fn
    pub span: Span,
    /// Whether the body contains a value-position anonymous `struct {..}` or
    /// `enum {..}` literal. Recorded by the parser, which has the only two
    /// production sites, so the definition index can skip the full
    /// `anonymous_type_sites` body walk for the overwhelming majority of
    /// declarations that contain none (RUE-1837).
    pub contains_anonymous_type_literal: bool,
}

/// A method definition in an impl block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    /// Directives applied to this method
    pub directives: Directives,
    /// Method name
    pub name: Ident,
    /// Whether this method takes self (None = associated function, Some = method with receiver)
    pub receiver: Option<SelfParam>,
    /// Method parameters (excluding self)
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`)
    pub return_type: Option<TypeExpr>,
    /// The optional place-returning qualifier and its keyword span.
    pub place_return: Option<PlaceReturn>,
    /// Method body
    pub body: Expr,
    /// Span covering the entire method
    pub span: Span,
    /// Whether the body contains a value-position anonymous `struct {..}` or
    /// `enum {..}` literal. Recorded by the parser, which has the only two
    /// production sites, so the definition index can skip the full
    /// `anonymous_type_sites` body walk for the overwhelming majority of
    /// declarations that contain none (RUE-1837).
    pub contains_anonymous_type_literal: bool,
}

/// A place-returning function result qualifier (ADR-0062).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceReturn {
    Borrow(Span),
    Inout(Span),
}

impl PlaceReturn {
    pub fn is_borrow(self) -> bool {
        matches!(self, Self::Borrow(_))
    }

    pub fn is_inout(self) -> bool {
        matches!(self, Self::Inout(_))
    }
}

/// A self parameter in a method.
///
/// The receiver mode mirrors the parameter modes (`borrow self` / `inout
/// self` / bare `self`), so the compiler can access `self` by reference for
/// borrow/inout receivers (RUE-15). Only `Normal`, `Borrow`, and `Inout` are
/// ever produced here — `Comptime` is not a valid receiver mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfParam {
    /// Receiver passing mode (`Normal` by-value, `Borrow`, or `Inout`).
    pub mode: ParamMode,
    /// Whether the receiver is declared `mut self`: the by-value receiver
    /// binds mutably in the method body, like `let mut`. Mutations affect
    /// only the callee's copy — there is no write-back to the caller (that
    /// is `inout self`). Only valid with `mode == Normal`; the parser never
    /// produces `is_mut` together with `Borrow`/`Inout`.
    pub is_mut: bool,
    /// Span covering the `self` keyword (and any leading mode keyword).
    pub span: Span,
}

/// Visibility of an item (function, struct, enum, etc.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Private to the current file (default)
    #[default]
    Private,
    /// Public - visible to importers
    Public,
}

/// A function definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Directives applied to this function
    pub directives: Directives,
    /// Visibility of this function
    pub visibility: Visibility,
    /// Whether this function is marked `unchecked` (can only be called from checked blocks)
    pub is_unchecked: bool,
    /// Function name
    pub name: Ident,
    /// Function parameters
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`)
    pub return_type: Option<TypeExpr>,
    /// The optional place-returning qualifier and its keyword span. Free
    /// functions retain the syntax so sema can diagnose the missing receiver.
    pub place_return: Option<PlaceReturn>,
    /// Function body
    pub body: Expr,
    /// The C ABI string when this function is a `pub extern "C" fn` export
    /// (ADR-0064 P4): `Some("C")` for an export exposed to C callers under its
    /// unmangled name, `None` for an ordinary Rue function. The slot reserves
    /// room for later ABI variants without a re-spelling, exactly as the import
    /// `extern` block does.
    pub export_abi: Option<String>,
    /// Span covering the entire function
    pub span: Span,
    /// Whether the body contains a value-position anonymous `struct {..}` or
    /// `enum {..}` literal. Recorded by the parser, which has the only two
    /// production sites, so the definition index can skip the full
    /// `anonymous_type_sites` body walk for the overwhelming majority of
    /// declarations that contain none (RUE-1837).
    pub contains_anonymous_type_literal: bool,
}

/// A foreign-declaration block: `extern "C" { fn getpid() -> i32; }`.
///
/// Groups body-less foreign function declarations that share an ABI. The ABI
/// string is a first-class part of the grammar (`"C"` is the only value the
/// current C FFI phase accepts, ADR-0064). Everything inside the block is a
/// foreign import lowered to an undefined linker symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternBlock {
    /// The ABI string as written (e.g. `"C"`).
    pub abi: String,
    /// The span of the ABI string literal, for diagnostics.
    pub abi_span: Span,
    /// The foreign function declarations in this block.
    pub fns: Vec<ExternFn>,
    /// Span covering the entire `extern` block.
    pub span: Span,
}

/// A single body-less foreign function declaration inside an [`ExternBlock`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternFn {
    /// The declared function name; also the undefined C symbol name (no
    /// mangling is applied to foreign declarations).
    pub name: Ident,
    /// Foreign function parameters.
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`).
    pub return_type: Option<TypeExpr>,
    /// Span covering the entire declaration.
    pub span: Span,
}

/// Parameter passing mode.
///
/// A parameter takes at most ONE mode keyword; the parser rejects repeated
/// or conflicting modifiers (e.g. `comptime comptime T` or `comptime inout
/// x`) with a targeted error. `ParamMode` is the single AST representation
/// of the `comptime` modifier — there is deliberately no separate
/// `is_comptime` flag (RUE-133 removed that dead duality).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamMode {
    /// Normal pass-by-value parameter
    Normal,
    /// Inout parameter - mutated in place and returned to caller
    Inout,
    /// Borrow parameter - immutable borrow without ownership transfer
    Borrow,
    /// Comptime parameter - evaluated at compile time (used for type
    /// parameters like `comptime T: type`)
    Comptime,
}

impl Default for ParamMode {
    fn default() -> Self {
        ParamMode::Normal
    }
}

impl ParamMode {
    /// The source keyword for this mode (empty for `Normal`).
    pub fn keyword(self) -> &'static str {
        match self {
            ParamMode::Normal => "",
            ParamMode::Inout => "inout",
            ParamMode::Borrow => "borrow",
            ParamMode::Comptime => "comptime",
        }
    }
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Parameter passing mode (normal, inout, borrow, or comptime)
    pub mode: ParamMode,
    /// Parameter name
    pub name: Ident,
    /// Parameter type
    pub ty: TypeExpr,
    /// Span covering the entire parameter
    pub span: Span,
}

/// An identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ident {
    pub name: Spur,
    pub span: Span,
}

/// A type expression in the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    /// A simple named type (e.g., i32, bool, MyStruct)
    Named(Ident),
    /// A module-qualified named type (e.g., std.option.OptionAlias).
    Qualified { segments: Vec<Ident>, span: Span },
    /// Unit type: ()
    Unit(Span),
    /// Never type: !
    Never(Span),
    /// Array type: [T; N] where T is the element type and N is the length.
    /// N may be an integer literal (`[i32; 4]`) or a name referring to a
    /// file-level `const` or a `comptime` value parameter (`[i32; N]`);
    /// named lengths are resolved to a compile-time constant during sema
    /// (RUE-16).
    Array {
        element: Box<TypeExpr>,
        length: ArrayLength,
        span: Span,
    },
    /// Slice type: `[T]` — a second-class fat-pointer view (ptr + runtime len)
    /// over a fixed array or growable buffer (ADR-0043, RUE-322). In parameter
    /// position the `borrow`/`inout` mode selects the shared/exclusive form
    /// (`borrow [T]` / `inout [T]`).
    Slice { element: Box<TypeExpr>, span: Span },
    /// Anonymous struct type: struct { field: Type, fn method(...) { ... }, ... }
    /// Used in comptime type construction (e.g., `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }`)
    /// Methods can be included inside the struct definition (Zig-style).
    AnonymousStruct {
        /// Field declarations (name and type)
        fields: Vec<AnonStructField>,
        /// Method definitions inside the anonymous struct
        methods: Vec<Method>,
        span: Span,
    },
    /// Anonymous enum type: enum { Variant1, Variant2(T), ... }
    /// The enum analog of `AnonymousStruct`, used in comptime type
    /// construction (e.g.
    /// `fn Option(comptime T: type) -> type { enum { Some(T), None } }`).
    /// This makes generic sum types like `Option`/`Result` expressible as
    /// ordinary comptime type functions (ADR-0038, RUE-6 phase 2).
    AnonymousEnum {
        /// Variant declarations (name and optional tuple payload types)
        variants: Vec<EnumVariant>,
        span: Span,
    },
    /// Raw pointer to immutable data: ptr const T
    PointerConst { pointee: Box<TypeExpr>, span: Span },
    /// Raw pointer to mutable data: ptr mut T
    PointerMut { pointee: Box<TypeExpr>, span: Span },
    /// A type-function application used directly in type position:
    /// `Name(arg, ...)` — e.g. `Result(i32, i32)` (RUE-241). This calls a
    /// comptime `-> type` function (a type constructor) with type arguments;
    /// sema reduces it to the monomorphized concrete type. The named-const
    /// form (`const R: type = Result(i32, i32); fn f() -> R`) resolves to the
    /// same type. Arguments are themselves type expressions, so nested calls
    /// (`Result(Option(i32), i32)`) compose.
    TypeCall {
        name: Ident,
        args: Vec<TypeExpr>,
        span: Span,
    },
    /// A module-qualified type-function application in type position:
    /// `std.option.Option(i64)`.
    QualifiedTypeCall {
        segments: Vec<Ident>,
        args: Vec<TypeExpr>,
        span: Span,
    },
    /// A fixed-capacity string type written with an integer-literal capacity:
    /// `Str(N)` where `N` is an integer literal (ADR-0043 Phase 5, RUE-326).
    /// `Str(N)` is the fixed string rung — `[u8; N]` + the UTF-8 byte-string
    /// convention — storing up to `N` bytes with no heap. This node exists only
    /// for the literal-capacity spelling (`Str(8)`); the const-capacity spelling
    /// (`Str(N)` where `N` names a `const`) parses as a `TypeCall` and reduces
    /// to the same canonical `Str(N)` type name. `name` is the callee ident (so
    /// a non-`Str` name like `Foo(8)` still resolves to a clean unknown-type
    /// error rather than being silently treated as a fixed string).
    StrFixed {
        name: Ident,
        length: u64,
        span: Span,
    },
    /// An integer literal in type-call ARGUMENT position: `Buffer(2)`,
    /// `Matrix(2, 3)`, `lib.Buffer(2)` (RUE-552). A type constructor may
    /// declare comptime VALUE parameters (`comptime N: i32`), so its
    /// application in type position takes value arguments alongside type
    /// arguments. Only produced inside `TypeCall`/`QualifiedTypeCall`
    /// argument lists; astgen canonicalizes it into the call's type string
    /// (the same decimal spelling `TypeExpr::StrFixed` produces for `Str(8)`).
    IntArg { value: i128, span: Span },
}

/// The length of an array type `[T; N]`.
///
/// `N` is either an integer literal or a name that refers to a compile-time
/// constant (a file-level `const` or a `comptime` value parameter). Named
/// lengths are resolved to a concrete `u64` during semantic analysis using the
/// const evaluator / comptime substitution machinery (RUE-16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayLength {
    /// A literal length, e.g. the `4` in `[i32; 4]`.
    Literal(u64),
    /// A named length, e.g. the `N` in `[i32; N]`.
    Named(Ident),
    /// A comptime-evaluable call in length position, e.g. the `fact(4)` in
    /// `[i32; fact(4)]` (RUE-309). The callee must be a value-returning
    /// function whose parameters are all `comptime`; sema folds the call to a
    /// concrete length via the same const evaluator that reduces `comptime`
    /// blocks (RUE-163). Arguments are themselves array-length expressions
    /// (a literal, a `const`/`comptime` name, or a nested call), so calls
    /// compose (`[i32; fact(g(2))]`).
    Call { name: Ident, args: Vec<ArrayLength> },
}

impl fmt::Display for ArrayLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArrayLength::Literal(n) => write!(f, "{}", n),
            // Match the canonical name encoding used elsewhere for identifiers.
            ArrayLength::Named(ident) => write!(f, "sym:{}", ident.name.into_usize()),
            ArrayLength::Call { name, args } => {
                write!(f, "sym:{}(", name.name.into_usize())?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")
            }
        }
    }
}

/// A field in an anonymous struct type expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonStructField {
    /// Field name
    pub name: Ident,
    /// Field type
    pub ty: TypeExpr,
    /// Span covering the entire field declaration
    pub span: Span,
}

impl TypeExpr {
    /// Get the span of this type expression.
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named(ident) => ident.span,
            TypeExpr::Qualified { span, .. } => *span,
            TypeExpr::Unit(span) => *span,
            TypeExpr::Never(span) => *span,
            TypeExpr::Array { span, .. } => *span,
            TypeExpr::Slice { span, .. } => *span,
            TypeExpr::AnonymousStruct { span, .. } => *span,
            TypeExpr::AnonymousEnum { span, .. } => *span,
            TypeExpr::PointerConst { span, .. } => *span,
            TypeExpr::PointerMut { span, .. } => *span,
            TypeExpr::TypeCall { span, .. } => *span,
            TypeExpr::QualifiedTypeCall { span, .. } => *span,
            TypeExpr::StrFixed { span, .. } => *span,
            TypeExpr::IntArg { span, .. } => *span,
        }
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeExpr::Named(ident) => write!(f, "sym:{}", ident.name.into_usize()),
            TypeExpr::Qualified { segments, .. } => {
                for (i, segment) in segments.iter().enumerate() {
                    if i > 0 {
                        write!(f, ".")?;
                    }
                    write!(f, "sym:{}", segment.name.into_usize())?;
                }
                Ok(())
            }
            TypeExpr::Unit(_) => write!(f, "()"),
            TypeExpr::Never(_) => write!(f, "!"),
            TypeExpr::Array {
                element, length, ..
            } => write!(f, "[{}; {}]", element, length),
            TypeExpr::Slice { element, .. } => write!(f, "[{}]", element),
            TypeExpr::AnonymousStruct {
                fields, methods, ..
            } => {
                write!(f, "struct {{ ")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "sym:{}: {}", field.name.name.into_usize(), field.ty)?;
                }
                for (i, method) in methods.iter().enumerate() {
                    if !fields.is_empty() || i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "fn sym:{}", method.name.name.into_usize())?;
                }
                write!(f, " }}")
            }
            TypeExpr::AnonymousEnum { variants, .. } => {
                write!(f, "enum {{ ")?;
                for (i, variant) in variants.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "sym:{}", variant.name.name.into_usize())?;
                    if !variant.payload.is_empty() {
                        write!(f, "(")?;
                        for (j, ty) in variant.payload.iter().enumerate() {
                            if j > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{}", ty)?;
                        }
                        write!(f, ")")?;
                    }
                }
                write!(f, " }}")
            }
            TypeExpr::PointerConst { pointee, .. } => write!(f, "ptr const {}", pointee),
            TypeExpr::PointerMut { pointee, .. } => write!(f, "ptr mut {}", pointee),
            TypeExpr::TypeCall { name, args, .. } => {
                write!(f, "sym:{}(", name.name.into_usize())?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")
            }
            TypeExpr::QualifiedTypeCall { segments, args, .. } => {
                for (i, segment) in segments.iter().enumerate() {
                    if i > 0 {
                        write!(f, ".")?;
                    }
                    write!(f, "sym:{}", segment.name.into_usize())?;
                }
                write!(f, "(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")
            }
            TypeExpr::StrFixed { name, length, .. } => {
                write!(f, "sym:{}({})", name.name.into_usize(), length)
            }
            TypeExpr::IntArg { value, .. } => write!(f, "{}", value),
        }
    }
}

/// A unit literal expression - represents `()` or implicit unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitLit {
    pub span: Span,
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Integer literal
    Int(IntLit),
    /// Floating-point literal (`1.5`, `1e9`) — an untyped `comptime_float`
    /// (ADR-0065 §3, RUE-1069)
    Float(FloatLit),
    /// String literal
    String(StringLit),
    /// Boolean literal
    Bool(BoolLit),
    /// Unit literal (explicit `()` or implicit unit for blocks without final expression)
    Unit(UnitLit),
    /// Identifier reference (variable)
    Ident(Ident),
    /// Binary operation (e.g., `a + b`)
    Binary(BinaryExpr),
    /// Unary operation (e.g., `-x`)
    Unary(UnaryExpr),
    /// Parenthesized expression (e.g., `(a + b)`)
    Paren(ParenExpr),
    /// Block with statements and an optional final expression.
    /// Blocks without an explicit final expression store implicit unit.
    Block(BlockExpr),
    /// If expression (e.g., `if cond { a } else { b }`)
    ///
    /// Boxed: `IfExpr` embeds both branch blocks inline, and at 120 bytes it
    /// was the variant that set `Expr`'s size for every other node (RUE-1836).
    If(Box<IfExpr>),
    /// Match expression (e.g., `match x { 1 => a, _ => b }`)
    Match(MatchExpr),
    /// While expression (e.g., `while cond { body }`)
    While(WhileExpr),
    /// Loop expression - infinite loop (e.g., `loop { body }`)
    Loop(LoopExpr),
    /// For expression - iterates over a built-in iterable
    /// (e.g., `for x in arr { body }`)
    For(ForExpr),
    /// Function call (e.g., `foo(1, 2)`)
    Call(CallExpr),
    /// Break statement (exits the innermost loop)
    Break(BreakExpr),
    /// Continue statement (skips to the next iteration of the innermost loop)
    Continue(ContinueExpr),
    /// Return statement (returns a value from the current function)
    Return(ReturnExpr),
    /// Yield statement (hands out a receiver projection from an accessor
    /// body, ADR-0062)
    Yield(YieldExpr),
    /// Struct literal (e.g., `Point { x: 1, y: 2 }`)
    StructLit(StructLitExpr),
    /// Field access (e.g., `point.x`)
    Field(FieldExpr),
    /// Method call (e.g., `point.distance()`)
    MethodCall(MethodCallExpr),
    /// Try/`?` propagation (e.g., `foo()?`): unwraps an `Option`, early-returning
    /// `None` from the enclosing (Option-returning) function on `None` (RUE-6).
    Try(TryExpr),
    /// Intrinsic call (e.g., `@dbg(42)`)
    IntrinsicCall(IntrinsicCallExpr),
    /// Array literal (e.g., `[1, 2, 3]`)
    ArrayLit(ArrayLitExpr),
    /// Array indexing (e.g., `arr[0]`)
    Index(IndexExpr),
    /// Path expression (e.g., `Color::Red`)
    Path(PathExpr),
    /// Self expression (e.g., `self` in method bodies)
    SelfExpr(SelfExpr),
    /// Comptime block expression (e.g., `comptime { 1 + 2 }`)
    Comptime(ComptimeBlockExpr),
    /// Checked block expression (e.g., `checked { @ptr_read(p) }`)
    Checked(CheckedBlockExpr),
    /// Type literal expression (e.g., `i32` used as a value in generic function calls)
    TypeLit(TypeLitExpr),
    /// Error node for recovered parse errors.
    /// Used by error recovery to continue parsing after a syntax error.
    Error(Span),
}

/// An integer literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntLit {
    pub value: u64,
    pub span: Span,
}

/// A floating-point literal.
///
/// `value` is the interned literal text with `_` separators removed, exactly
/// as the lexer produced it (`1.5`, `1e9`, `6.022e23`). The literal is an
/// untyped `comptime_float` — arbitrary precision until context picks `f32` or
/// `f64` — so no decoding happens here; the phase that knows the target width
/// parses the text (ADR-0065 §3, RUE-1069).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatLit {
    pub value: Spur,
    pub span: Span,
}

/// A string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringLit {
    pub value: Spur,
    pub span: Span,
}

/// A boolean literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoolLit {
    pub value: bool,
    pub span: Span,
}

/// A binary expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryExpr {
    pub left: Box<Expr>,
    pub op: BinaryOp,
    pub right: Box<Expr>,
    pub span: Span,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    // Arithmetic
    Add, // +
    Sub, // -
    Mul, // *
    Div, // /
    Mod, // %
    // Comparison
    Eq, // ==
    Ne, // !=
    Lt, // <
    Gt, // >
    Le, // <=
    Ge, // >=
    // Logical
    And, // &&
    Or,  // ||
    // Bitwise
    BitAnd, // &
    BitOr,  // |
    BitXor, // ^
    Shl,    // <<
    Shr,    // >>
}

/// A unary expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnaryExpr {
    pub op: UnaryOp,
    pub operand: Box<Expr>,
    pub span: Span,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,    // -
    Not,    // !
    BitNot, // ~
}

/// A parenthesized expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParenExpr {
    pub inner: Box<Expr>,
    pub span: Span,
}

/// A block expression containing statements and a value expression.
///
/// A source block may omit its final expression; in that case the parser stores
/// an implicit unit expression here so later phases always have a block value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockExpr {
    /// Statements in the block
    pub statements: Vec<Statement>,
    /// Value of the block, either the explicit final expression or implicit unit.
    pub expr: Box<Expr>,
    pub span: Span,
}

/// An if expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfExpr {
    /// Condition (must be bool)
    pub cond: Box<Expr>,
    /// Then branch
    pub then_block: BlockExpr,
    /// Optional else branch
    pub else_block: Option<BlockExpr>,
    pub span: Span,
}

/// A match expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchExpr {
    /// The value being matched (scrutinee)
    pub scrutinee: Box<Expr>,
    /// Match arms
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

/// A single arm in a match expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    /// The pattern to match
    pub pattern: Pattern,
    /// The body expression
    pub body: Box<Expr>,
    pub span: Span,
}

/// A pattern in a match arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard(Span),
    /// Integer literal pattern (positive or zero)
    Int(IntLit),
    /// Negative integer literal pattern (e.g., `-1`, `-42`)
    NegInt(NegIntLit),
    /// Boolean literal pattern
    Bool(BoolLit),
    /// Path pattern (e.g., `Color::Red` for enum variant)
    Path(PathPattern),
}

/// A negative integer literal pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegIntLit {
    /// The absolute value of the negative integer
    pub value: u64,
    /// Span covering the entire pattern (minus sign and literal)
    pub span: Span,
}

/// A path pattern (e.g., `Color::Red` or `module.Color::Red` for enum variant matching).
///
/// A tuple-variant pattern binds the variant's payload into fresh names:
/// `Circle(r)`, `Rect(w, h)`. The `bindings` vector holds those binding names
/// in payload order; it is empty for a discriminant-only pattern (`Color::Red`).
/// Payload bindings require the `enum_payloads` preview feature (RUE-221).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPattern {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Color::Red`)
    pub base: Option<Box<Expr>>,
    /// The type name (e.g., `Color`)
    pub type_name: Ident,
    /// Inline type-constructor arguments when the pattern head is a
    /// type-constructor call, e.g. `Result(i32, i32).Ok(v)` (RUE-596,
    /// spec 4.14:23). When `Some`, `type_name`
    /// is the constructor function and these are its comptime arguments; the
    /// pattern's enum type is the reduction of `type_name(ctor_args)`. `None`
    /// for an ordinary `Enum.Variant` pattern.
    pub ctor_args: Option<Vec<CallArg>>,
    /// The variant name (e.g., `Red`)
    pub variant: Ident,
    /// Payload binding names (empty = no payload pattern).
    pub bindings: Vec<Ident>,
    pub span: Span,
}

impl Pattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard(span) => *span,
            Pattern::Int(lit) => lit.span,
            Pattern::NegInt(lit) => lit.span,
            Pattern::Bool(lit) => lit.span,
            Pattern::Path(path) => path.span,
        }
    }
}

/// Argument passing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArgMode {
    /// Normal pass-by-value argument
    #[default]
    Normal,
    /// Inout argument - mutated in place
    Inout,
    /// Borrow argument - immutable borrow
    Borrow,
}

/// An argument in a function call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallArg {
    /// The passing mode for this argument
    pub mode: ArgMode,
    /// The argument expression
    pub expr: Expr,
    /// Span covering the entire argument (including inout/borrow keyword if present)
    pub span: Span,
}

impl CallArg {
    /// Returns true when this argument uses `inout` passing mode.
    pub fn is_inout(&self) -> bool {
        self.mode == ArgMode::Inout
    }

    /// Returns true if this argument is passed as borrow.
    pub fn is_borrow(&self) -> bool {
        self.mode == ArgMode::Borrow
    }
}

/// A function call expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallExpr {
    /// Function name
    pub name: Ident,
    /// Arguments
    pub args: Vec<CallArg>,
    pub span: Span,
}

/// An argument to an intrinsic call (can be an expression or a type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntrinsicArg {
    /// An expression argument (e.g., `@dbg(42)`)
    Expr(Expr),
    /// A type argument (e.g., `@size_of(i32)`)
    Type(TypeExpr),
}

/// An intrinsic call expression (e.g., `@dbg(42)` or `@size_of(i32)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrinsicCallExpr {
    /// Intrinsic name (without the @)
    pub name: Ident,
    /// Arguments (can be expressions or types)
    pub args: Vec<IntrinsicArg>,
    pub span: Span,
}

/// A struct literal expression (e.g., `Point { x: 1, y: 2 }` or `module.Point { x: 1, y: 2 }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLitExpr {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Point { ... }`)
    pub base: Option<Box<Expr>>,
    /// Struct type name
    pub name: Ident,
    /// Inline type-constructor arguments when the head is a type-constructor
    /// call, e.g. `Pair(i32) { ... }` (RUE-596, spec 4.14:23). When `Some`,
    /// `name` is the constructor function
    /// and these are its comptime arguments; the literal's struct type is the
    /// reduction of `name(ctor_args)`. `None` for an ordinary `Name { ... }`.
    pub ctor_args: Option<Vec<CallArg>>,
    /// Field initializers
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

/// A field initializer in a struct literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInit {
    /// Field name
    pub name: Ident,
    /// Field value
    pub value: Box<Expr>,
    /// Whether this initializer used field-init shorthand (`P { x }` desugaring
    /// to `P { x: x }`, RUE-613, stabilized in RUE-628). When `true`, `value` is
    /// the desugared `Expr::Ident(name)`; the flag is preserved so diagnostics
    /// can point at the shorthand.
    pub shorthand: bool,
    pub span: Span,
}

/// A field access expression (e.g., `point.x`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldExpr {
    /// Base expression (the struct value)
    pub base: Box<Expr>,
    /// Field name
    pub field: Ident,
    pub span: Span,
}

/// A try/`?` propagation expression (e.g., `foo()?`).
///
/// `operand` must evaluate to an `Option`; the `?` unwraps it to the `Some`
/// payload, early-returning `None` from the enclosing function when the operand
/// is `None` (RUE-6, ADR-0038). The enclosing function must itself return an
/// `Option`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TryExpr {
    /// The operand whose `Option` is unwrapped/propagated.
    pub operand: Box<Expr>,
    /// Span covering `operand?` (through the `?`).
    pub span: Span,
}

/// A method call expression (e.g., `point.distance()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodCallExpr {
    /// Base expression (the receiver)
    pub receiver: Box<Expr>,
    /// Method name
    pub method: Ident,
    /// Arguments (excluding self)
    pub args: Vec<CallArg>,
    pub span: Span,
}

/// An array literal expression.
///
/// Two forms share this node:
/// * List form `[1, 2, 3]`: `elements` holds every element and `repeat` is
///   `None`.
/// * Repeat form `[value; count]` (RUE-235): `elements` holds the single
///   `value` expression and `repeat` holds the `count`. The count is a
///   compile-time constant (an integer literal or a named `const` /
///   `comptime` value parameter), resolved during semantic analysis via the
///   same const-eval path as array-type lengths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayLitExpr {
    /// Array elements. For the repeat form this holds exactly one element: the
    /// value being repeated.
    pub elements: Vec<Expr>,
    /// For the repeat form `[value; count]`, the repeat count; `None` for the
    /// list form.
    pub repeat: Option<ArrayLength>,
    pub span: Span,
}

/// An array index expression (e.g., `arr[0]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexExpr {
    /// The array being indexed
    pub base: Box<Expr>,
    /// The index expression
    pub index: Box<Expr>,
    pub span: Span,
}

/// A path expression (e.g., `Color::Red` or `module.Color::Red` for enum variant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathExpr {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Color::Red`)
    pub base: Option<Box<Expr>>,
    /// The type name (e.g., `Color`)
    pub type_name: Ident,
    /// The variant name (e.g., `Red`)
    pub variant: Ident,
    pub span: Span,
}

/// A statement (does not produce a value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// Let binding: `let x = expr;` or `let mut x = expr;`
    Let(LetStatement),
    /// Assignment: `x = expr;`
    Assign(AssignStatement),
    /// Expression statement: `expr;`
    Expr(Expr),
}

/// A pattern in a let binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LetPattern {
    /// Named binding (e.g., `x`, `_unused`)
    Ident(Ident),
    /// Wildcard pattern `_` - discards the value without creating a binding
    Wildcard(Span),
}

impl LetPattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            LetPattern::Ident(ident) => ident.span,
            LetPattern::Wildcard(span) => *span,
        }
    }
}

/// A let binding statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetStatement {
    /// Directives applied to this let binding.
    ///
    /// Boxed and optional: `Directives` is a `SmallVec` with one inline
    /// `Directive`, so it costs 72 bytes on every `let` whether or not any
    /// directive is present — and almost none are (RUE-1836). `None` is the
    /// empty case; use [`LetStatement::directives`] to read either as a slice.
    pub directives: Option<Box<Directives>>,
    /// Whether the binding is mutable
    pub is_mut: bool,
    /// The binding pattern (identifier or wildcard)
    pub pattern: LetPattern,
    /// Optional type annotation.
    ///
    /// Boxed: an inline `TypeExpr` costs 64 bytes on every `let`, annotated or
    /// not, and a `Vec<Statement>` block body pays it per statement (RUE-1836).
    pub ty: Option<Box<TypeExpr>>,
    /// Initializer expression
    pub init: Box<Expr>,
    pub span: Span,
}

impl LetStatement {
    /// The directives on this binding, empty when there are none.
    pub fn directives(&self) -> &[Directive] {
        self.directives.as_ref().map_or(&[], |d| d.as_slice())
    }
}

/// An assignment statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignStatement {
    /// Assignment target (variable or field)
    pub target: AssignTarget,
    /// The compound-assignment operator, when the statement was written as
    /// `place op= value` (RUE-1043). `None` for a plain `place = value`.
    pub op: Option<CompoundOp>,
    /// Value expression
    pub value: Box<Expr>,
    pub span: Span,
}

/// The operator of a compound assignment statement `place op= value` (RUE-1043).
///
/// Only the binary operators that produce a value of the target's own type can
/// appear here; the comparison and short-circuiting logical operators have no
/// compound form because their result type differs from their operands'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundOp {
    Add,    // +=
    Sub,    // -=
    Mul,    // *=
    Div,    // /=
    Mod,    // %=
    BitAnd, // &=
    BitOr,  // |=
    BitXor, // ^=
    Shl,    // <<=
    Shr,    // >>=
}

impl CompoundOp {
    /// The compound operator spelled by `kind`, if it spells one.
    pub fn from_token(kind: rue_lexer::TokenKind) -> Option<Self> {
        use rue_lexer::TokenKind as T;
        Some(match kind {
            T::PlusEq => CompoundOp::Add,
            T::MinusEq => CompoundOp::Sub,
            T::StarEq => CompoundOp::Mul,
            T::SlashEq => CompoundOp::Div,
            T::PercentEq => CompoundOp::Mod,
            T::AmpEq => CompoundOp::BitAnd,
            T::PipeEq => CompoundOp::BitOr,
            T::CaretEq => CompoundOp::BitXor,
            T::LtLtEq => CompoundOp::Shl,
            T::GtGtEq => CompoundOp::Shr,
            _ => return None,
        })
    }

    /// The source spelling of the compound operator, e.g. `+=`.
    pub fn spelling(self) -> &'static str {
        match self {
            CompoundOp::Add => "+=",
            CompoundOp::Sub => "-=",
            CompoundOp::Mul => "*=",
            CompoundOp::Div => "/=",
            CompoundOp::Mod => "%=",
            CompoundOp::BitAnd => "&=",
            CompoundOp::BitOr => "|=",
            CompoundOp::BitXor => "^=",
            CompoundOp::Shl => "<<=",
            CompoundOp::Shr => ">>=",
        }
    }
}

/// An assignment target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignTarget {
    /// Variable assignment (e.g., `x = 5`)
    Var(Ident),
    /// Field assignment (e.g., `point.x = 5`)
    Field(FieldExpr),
    /// Index assignment (e.g., `arr[0] = 5`)
    Index(IndexExpr),
    /// Direct assignment to a place-returning accessor result.
    Method(Box<Expr>),
}

/// A while loop expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhileExpr {
    /// Condition (must be bool)
    pub cond: Box<Expr>,
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// An infinite loop expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopExpr {
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// A `for` expression that iterates over a built-in iterable (RUE-220).
///
/// `for <binder> in <iterable> { body }` iterates in read/borrow mode over one
/// of the compiler-known iterables (an array, a String's bytes, or a String's
/// `.chars()` view). It is lowered to a scoped-borrow + position + `loop`
/// desugaring in AstGen (see `gen_for`); there is no borrow-holding iterator
/// object. The `.chars()` char view is recognized syntactically at lowering
/// time, so the AST just stores the iterable expression as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForExpr {
    /// The loop-variable binding (identifier or `_` wildcard).
    pub binder: LetPattern,
    /// The iterable expression (`in <iterable>`).
    pub iterable: Box<Expr>,
    /// Loop body.
    pub body: BlockExpr,
    pub span: Span,
}

/// A break expression (exits the innermost loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakExpr {
    /// A value operand (e.g. `break 42`). Parsed for diagnostics, but always
    /// rejected by semantic analysis: break does not carry a value.
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

/// A continue expression (skips to the next iteration of the innermost loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueExpr {
    pub span: Span,
}

/// A return expression (returns a value from the current function).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnExpr {
    /// The value to return (None for `return;` in unit-returning functions)
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

/// A yield expression: the exit form of a `-> borrow T` accessor body
/// (ADR-0062). Its operand is the place the accessor hands out; unlike
/// `return` the operand is mandatory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YieldExpr {
    /// The place expression the accessor yields.
    pub value: Box<Expr>,
    pub span: Span,
}

/// A self expression (the `self` keyword in method bodies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfExpr {
    pub span: Span,
}

/// A comptime block expression (e.g., `comptime { 1 + 2 }`).
/// The expression inside must be evaluable at compile time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComptimeBlockExpr {
    /// The expression to evaluate at compile time
    pub expr: Box<Expr>,
    pub span: Span,
}

/// A checked block expression (e.g., `checked { @ptr_read(p) }`).
/// Unchecked operations (raw pointer manipulation, calling unchecked functions)
/// are only allowed inside checked blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedBlockExpr {
    /// The expression inside the checked block
    pub expr: Box<Expr>,
    pub span: Span,
}

/// A type literal expression (e.g., `i32` used as a value).
/// This represents a type used as a value in expression context, typically
/// as an argument to a generic function with comptime parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeLitExpr {
    /// The type being used as a value
    pub type_expr: TypeExpr,
    pub span: Span,
}

impl Expr {
    /// Get the span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(lit) => lit.span,
            Expr::Float(lit) => lit.span,
            Expr::String(lit) => lit.span,
            Expr::Bool(lit) => lit.span,
            Expr::Unit(lit) => lit.span,
            Expr::Ident(ident) => ident.span,
            Expr::Binary(bin) => bin.span,
            Expr::Unary(un) => un.span,
            Expr::Paren(paren) => paren.span,
            Expr::Block(block) => block.span,
            Expr::If(if_expr) => if_expr.span,
            Expr::Match(match_expr) => match_expr.span,
            Expr::While(while_expr) => while_expr.span,
            Expr::Loop(loop_expr) => loop_expr.span,
            Expr::For(for_expr) => for_expr.span,
            Expr::Call(call) => call.span,
            Expr::Break(break_expr) => break_expr.span,
            Expr::Continue(continue_expr) => continue_expr.span,
            Expr::Return(return_expr) => return_expr.span,
            Expr::Yield(yield_expr) => yield_expr.span,
            Expr::StructLit(struct_lit) => struct_lit.span,
            Expr::Field(field_expr) => field_expr.span,
            Expr::MethodCall(method_call) => method_call.span,
            Expr::Try(try_expr) => try_expr.span,
            Expr::IntrinsicCall(intrinsic) => intrinsic.span,
            Expr::ArrayLit(array_lit) => array_lit.span,
            Expr::Index(index_expr) => index_expr.span,
            Expr::Path(path_expr) => path_expr.span,
            Expr::SelfExpr(self_expr) => self_expr.span,
            Expr::Comptime(comptime_expr) => comptime_expr.span,
            Expr::Checked(checked_expr) => checked_expr.span,
            Expr::TypeLit(type_lit) => type_lit.span,
            Expr::Error(span) => *span,
        }
    }

    /// Append this expression's direct sub-expressions to `out`.
    ///
    /// The match is exhaustive with no catch-all arm, so a new [`Expr`]
    /// variant does not compile until its sub-expressions are listed here.
    /// That is what lets a consumer decide a containment question over a body
    /// — "does this accessor body contain a `return`?" (spec 6.6:6) — from
    /// syntax alone, with no chance of silently missing a form.
    ///
    /// Statements are traversed through the blocks that hold them. Types are
    /// not expressions and are never reported, so an anonymous struct type
    /// written inside a body keeps its own method bodies to itself.
    pub fn child_exprs<'a>(&'a self, out: &mut Vec<&'a Expr>) {
        fn block<'a>(block: &'a BlockExpr, out: &mut Vec<&'a Expr>) {
            for statement in &block.statements {
                match statement {
                    Statement::Let(binding) => out.push(&binding.init),
                    Statement::Assign(assignment) => {
                        match &assignment.target {
                            AssignTarget::Var(_) => {}
                            AssignTarget::Field(field) => out.push(&field.base),
                            AssignTarget::Index(index) => {
                                out.extend([index.base.as_ref(), index.index.as_ref()])
                            }
                            AssignTarget::Method(expr) => out.push(expr),
                        }
                        out.push(&assignment.value);
                    }
                    Statement::Expr(expr) => out.push(expr),
                }
            }
            out.push(&block.expr);
        }
        fn args<'a>(args: &'a [CallArg], out: &mut Vec<&'a Expr>) {
            out.extend(args.iter().map(|arg| &arg.expr));
        }
        match self {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Bool(_)
            | Expr::Unit(_)
            | Expr::Ident(_)
            | Expr::Continue(_)
            | Expr::SelfExpr(_)
            | Expr::TypeLit(_)
            | Expr::Error(_) => {}
            Expr::Binary(binary) => out.extend([binary.left.as_ref(), binary.right.as_ref()]),
            Expr::Unary(unary) => out.push(&unary.operand),
            Expr::Paren(paren) => out.push(&paren.inner),
            Expr::Block(body) => block(body, out),
            Expr::If(expr) => {
                out.push(&expr.cond);
                block(&expr.then_block, out);
                if let Some(otherwise) = &expr.else_block {
                    block(otherwise, out);
                }
            }
            Expr::Match(expr) => {
                out.push(&expr.scrutinee);
                out.extend(expr.arms.iter().map(|arm| arm.body.as_ref()));
            }
            Expr::While(expr) => {
                out.push(&expr.cond);
                block(&expr.body, out);
            }
            Expr::Loop(expr) => block(&expr.body, out),
            Expr::For(expr) => {
                out.push(&expr.iterable);
                block(&expr.body, out);
            }
            Expr::Call(call) => args(&call.args, out),
            Expr::Break(expr) => out.extend(expr.value.as_deref()),
            Expr::Return(expr) => out.extend(expr.value.as_deref()),
            Expr::Yield(expr) => out.push(&expr.value),
            Expr::StructLit(literal) => {
                out.extend(literal.base.as_deref());
                if let Some(ctor_args) = &literal.ctor_args {
                    args(ctor_args, out);
                }
                out.extend(literal.fields.iter().map(|field| field.value.as_ref()));
            }
            Expr::Field(field) => out.push(&field.base),
            Expr::MethodCall(call) => {
                out.push(&call.receiver);
                args(&call.args, out);
            }
            Expr::Try(expr) => out.push(&expr.operand),
            Expr::IntrinsicCall(call) => out.extend(call.args.iter().filter_map(|arg| match arg {
                IntrinsicArg::Expr(expr) => Some(expr),
                IntrinsicArg::Type(_) => None,
            })),
            Expr::ArrayLit(literal) => out.extend(literal.elements.iter()),
            Expr::Index(index) => out.extend([index.base.as_ref(), index.index.as_ref()]),
            Expr::Path(path) => out.extend(path.base.as_deref()),
            Expr::Comptime(expr) => out.push(&expr.expr),
            Expr::Checked(expr) => out.push(&expr.expr),
        }
    }
}

fn rebind_span(span: &mut Span, file_id: FileId) {
    span.file_id = file_id;
}

fn rebind_ident(ident: &mut Ident, file_id: FileId) {
    rebind_span(&mut ident.span, file_id);
}

fn rebind_directives(directives: &mut Directives, file_id: FileId) {
    for directive in directives {
        rebind_ident(&mut directive.name, file_id);
        for argument in &mut directive.args {
            match argument {
                DirectiveArg::Ident(ident) => rebind_ident(ident, file_id),
            }
        }
        rebind_span(&mut directive.span, file_id);
    }
}

fn rebind_item(item: &mut Item, file_id: FileId) {
    match item {
        Item::Function(function) => {
            rebind_directives(&mut function.directives, file_id);
            rebind_ident(&mut function.name, file_id);
            for parameter in &mut function.params {
                rebind_param(parameter, file_id);
            }
            if let Some(return_type) = &mut function.return_type {
                rebind_type(return_type, file_id);
            }
            rebind_expr(&mut function.body, file_id);
            rebind_span(&mut function.span, file_id);
        }
        Item::Struct(structure) => {
            rebind_directives(&mut structure.directives, file_id);
            rebind_ident(&mut structure.name, file_id);
            for field in &mut structure.fields {
                rebind_ident(&mut field.name, file_id);
                rebind_type(&mut field.ty, file_id);
                rebind_span(&mut field.span, file_id);
            }
            for method in &mut structure.methods {
                rebind_method(method, file_id);
            }
            rebind_span(&mut structure.span, file_id);
        }
        Item::Enum(enumeration) => {
            rebind_directives(&mut enumeration.directives, file_id);
            rebind_ident(&mut enumeration.name, file_id);
            for variant in &mut enumeration.variants {
                rebind_enum_variant(variant, file_id);
            }
            rebind_span(&mut enumeration.span, file_id);
        }
        Item::DropFn(drop_fn) => {
            rebind_ident(&mut drop_fn.type_name, file_id);
            rebind_self_param(&mut drop_fn.self_param, file_id);
            rebind_expr(&mut drop_fn.body, file_id);
            rebind_span(&mut drop_fn.span, file_id);
        }
        Item::Extern(extern_block) => {
            rebind_span(&mut extern_block.abi_span, file_id);
            for foreign in &mut extern_block.fns {
                rebind_ident(&mut foreign.name, file_id);
                for parameter in &mut foreign.params {
                    rebind_param(parameter, file_id);
                }
                if let Some(return_type) = &mut foreign.return_type {
                    rebind_type(return_type, file_id);
                }
                rebind_span(&mut foreign.span, file_id);
            }
            rebind_span(&mut extern_block.span, file_id);
        }
        Item::Const(constant) => {
            rebind_directives(&mut constant.directives, file_id);
            rebind_ident(&mut constant.name, file_id);
            if let Some(ty) = &mut constant.ty {
                rebind_type(ty, file_id);
            }
            rebind_expr(&mut constant.init, file_id);
            rebind_span(&mut constant.span, file_id);
        }
        Item::Error(span) => rebind_span(span, file_id),
    }
}

fn rebind_method(method: &mut Method, file_id: FileId) {
    rebind_directives(&mut method.directives, file_id);
    rebind_ident(&mut method.name, file_id);
    if let Some(receiver) = &mut method.receiver {
        rebind_self_param(receiver, file_id);
    }
    for parameter in &mut method.params {
        rebind_param(parameter, file_id);
    }
    if let Some(return_type) = &mut method.return_type {
        rebind_type(return_type, file_id);
    }
    rebind_expr(&mut method.body, file_id);
    rebind_span(&mut method.span, file_id);
}

fn rebind_self_param(parameter: &mut SelfParam, file_id: FileId) {
    rebind_span(&mut parameter.span, file_id);
}

fn rebind_param(parameter: &mut Param, file_id: FileId) {
    rebind_ident(&mut parameter.name, file_id);
    rebind_type(&mut parameter.ty, file_id);
    rebind_span(&mut parameter.span, file_id);
}

fn rebind_enum_variant(variant: &mut EnumVariant, file_id: FileId) {
    rebind_ident(&mut variant.name, file_id);
    for payload in &mut variant.payload {
        rebind_type(payload, file_id);
    }
    rebind_span(&mut variant.span, file_id);
}

fn rebind_type(ty: &mut TypeExpr, file_id: FileId) {
    match ty {
        TypeExpr::Named(ident) => rebind_ident(ident, file_id),
        TypeExpr::Qualified { segments, span } => {
            for segment in segments {
                rebind_ident(segment, file_id);
            }
            rebind_span(span, file_id);
        }
        TypeExpr::Unit(span) | TypeExpr::Never(span) => rebind_span(span, file_id),
        TypeExpr::Array {
            element,
            length,
            span,
        } => {
            rebind_type(element, file_id);
            rebind_array_length(length, file_id);
            rebind_span(span, file_id);
        }
        TypeExpr::Slice { element, span } => {
            rebind_type(element, file_id);
            rebind_span(span, file_id);
        }
        TypeExpr::AnonymousStruct {
            fields,
            methods,
            span,
        } => {
            for field in fields {
                rebind_ident(&mut field.name, file_id);
                rebind_type(&mut field.ty, file_id);
                rebind_span(&mut field.span, file_id);
            }
            for method in methods {
                rebind_method(method, file_id);
            }
            rebind_span(span, file_id);
        }
        TypeExpr::AnonymousEnum { variants, span } => {
            for variant in variants {
                rebind_enum_variant(variant, file_id);
            }
            rebind_span(span, file_id);
        }
        TypeExpr::PointerConst { pointee, span } | TypeExpr::PointerMut { pointee, span } => {
            rebind_type(pointee, file_id);
            rebind_span(span, file_id);
        }
        TypeExpr::TypeCall { name, args, span } => {
            rebind_ident(name, file_id);
            for argument in args {
                rebind_type(argument, file_id);
            }
            rebind_span(span, file_id);
        }
        TypeExpr::QualifiedTypeCall {
            segments,
            args,
            span,
        } => {
            for segment in segments {
                rebind_ident(segment, file_id);
            }
            for argument in args {
                rebind_type(argument, file_id);
            }
            rebind_span(span, file_id);
        }
        TypeExpr::StrFixed { name, span, .. } => {
            rebind_ident(name, file_id);
            rebind_span(span, file_id);
        }
        TypeExpr::IntArg { span, .. } => rebind_span(span, file_id),
    }
}

fn rebind_array_length(length: &mut ArrayLength, file_id: FileId) {
    match length {
        ArrayLength::Literal(_) => {}
        ArrayLength::Named(ident) => rebind_ident(ident, file_id),
        ArrayLength::Call { name, args } => {
            rebind_ident(name, file_id);
            for argument in args {
                rebind_array_length(argument, file_id);
            }
        }
    }
}

fn rebind_pattern(pattern: &mut Pattern, file_id: FileId) {
    match pattern {
        Pattern::Wildcard(span) => rebind_span(span, file_id),
        Pattern::Int(literal) => rebind_span(&mut literal.span, file_id),
        Pattern::NegInt(literal) => rebind_span(&mut literal.span, file_id),
        Pattern::Bool(literal) => rebind_span(&mut literal.span, file_id),
        Pattern::Path(path) => {
            if let Some(base) = &mut path.base {
                rebind_expr(base, file_id);
            }
            rebind_ident(&mut path.type_name, file_id);
            if let Some(arguments) = &mut path.ctor_args {
                for argument in arguments {
                    rebind_call_arg(argument, file_id);
                }
            }
            rebind_ident(&mut path.variant, file_id);
            for binding in &mut path.bindings {
                rebind_ident(binding, file_id);
            }
            rebind_span(&mut path.span, file_id);
        }
    }
}

fn rebind_call_arg(argument: &mut CallArg, file_id: FileId) {
    rebind_expr(&mut argument.expr, file_id);
    rebind_span(&mut argument.span, file_id);
}

fn rebind_block(block: &mut BlockExpr, file_id: FileId) {
    for statement in &mut block.statements {
        rebind_statement(statement, file_id);
    }
    rebind_expr(&mut block.expr, file_id);
    rebind_span(&mut block.span, file_id);
}

fn rebind_expr(expr: &mut Expr, file_id: FileId) {
    match expr {
        Expr::Int(literal) => rebind_span(&mut literal.span, file_id),
        Expr::Float(literal) => rebind_span(&mut literal.span, file_id),
        Expr::String(literal) => rebind_span(&mut literal.span, file_id),
        Expr::Bool(literal) => rebind_span(&mut literal.span, file_id),
        Expr::Unit(literal) => rebind_span(&mut literal.span, file_id),
        Expr::Ident(ident) => rebind_ident(ident, file_id),
        Expr::Binary(binary) => {
            rebind_expr(&mut binary.left, file_id);
            rebind_expr(&mut binary.right, file_id);
            rebind_span(&mut binary.span, file_id);
        }
        Expr::Unary(unary) => {
            rebind_expr(&mut unary.operand, file_id);
            rebind_span(&mut unary.span, file_id);
        }
        Expr::Paren(paren) => {
            rebind_expr(&mut paren.inner, file_id);
            rebind_span(&mut paren.span, file_id);
        }
        Expr::Block(block) => rebind_block(block, file_id),
        Expr::If(if_expr) => {
            rebind_expr(&mut if_expr.cond, file_id);
            rebind_block(&mut if_expr.then_block, file_id);
            if let Some(else_block) = &mut if_expr.else_block {
                rebind_block(else_block, file_id);
            }
            rebind_span(&mut if_expr.span, file_id);
        }
        Expr::Match(match_expr) => {
            rebind_expr(&mut match_expr.scrutinee, file_id);
            for arm in &mut match_expr.arms {
                rebind_pattern(&mut arm.pattern, file_id);
                rebind_expr(&mut arm.body, file_id);
                rebind_span(&mut arm.span, file_id);
            }
            rebind_span(&mut match_expr.span, file_id);
        }
        Expr::While(while_expr) => {
            rebind_expr(&mut while_expr.cond, file_id);
            rebind_block(&mut while_expr.body, file_id);
            rebind_span(&mut while_expr.span, file_id);
        }
        Expr::Loop(loop_expr) => {
            rebind_block(&mut loop_expr.body, file_id);
            rebind_span(&mut loop_expr.span, file_id);
        }
        Expr::For(for_expr) => {
            rebind_let_pattern(&mut for_expr.binder, file_id);
            rebind_expr(&mut for_expr.iterable, file_id);
            rebind_block(&mut for_expr.body, file_id);
            rebind_span(&mut for_expr.span, file_id);
        }
        Expr::Call(call) => {
            rebind_ident(&mut call.name, file_id);
            for argument in &mut call.args {
                rebind_call_arg(argument, file_id);
            }
            rebind_span(&mut call.span, file_id);
        }
        Expr::Break(break_expr) => {
            if let Some(value) = &mut break_expr.value {
                rebind_expr(value, file_id);
            }
            rebind_span(&mut break_expr.span, file_id);
        }
        Expr::Continue(continue_expr) => rebind_span(&mut continue_expr.span, file_id),
        Expr::Return(return_expr) => {
            if let Some(value) = &mut return_expr.value {
                rebind_expr(value, file_id);
            }
            rebind_span(&mut return_expr.span, file_id);
        }
        Expr::Yield(yield_expr) => {
            rebind_expr(&mut yield_expr.value, file_id);
            rebind_span(&mut yield_expr.span, file_id);
        }
        Expr::StructLit(literal) => {
            if let Some(base) = &mut literal.base {
                rebind_expr(base, file_id);
            }
            rebind_ident(&mut literal.name, file_id);
            if let Some(arguments) = &mut literal.ctor_args {
                for argument in arguments {
                    rebind_call_arg(argument, file_id);
                }
            }
            for field in &mut literal.fields {
                rebind_ident(&mut field.name, file_id);
                rebind_expr(&mut field.value, file_id);
                rebind_span(&mut field.span, file_id);
            }
            rebind_span(&mut literal.span, file_id);
        }
        Expr::Field(field) => {
            rebind_expr(&mut field.base, file_id);
            rebind_ident(&mut field.field, file_id);
            rebind_span(&mut field.span, file_id);
        }
        Expr::MethodCall(call) => {
            rebind_expr(&mut call.receiver, file_id);
            rebind_ident(&mut call.method, file_id);
            for argument in &mut call.args {
                rebind_call_arg(argument, file_id);
            }
            rebind_span(&mut call.span, file_id);
        }
        Expr::Try(try_expr) => {
            rebind_expr(&mut try_expr.operand, file_id);
            rebind_span(&mut try_expr.span, file_id);
        }
        Expr::IntrinsicCall(call) => {
            rebind_ident(&mut call.name, file_id);
            for argument in &mut call.args {
                match argument {
                    IntrinsicArg::Expr(expr) => rebind_expr(expr, file_id),
                    IntrinsicArg::Type(ty) => rebind_type(ty, file_id),
                }
            }
            rebind_span(&mut call.span, file_id);
        }
        Expr::ArrayLit(array) => {
            for element in &mut array.elements {
                rebind_expr(element, file_id);
            }
            if let Some(repeat) = &mut array.repeat {
                rebind_array_length(repeat, file_id);
            }
            rebind_span(&mut array.span, file_id);
        }
        Expr::Index(index) => {
            rebind_expr(&mut index.base, file_id);
            rebind_expr(&mut index.index, file_id);
            rebind_span(&mut index.span, file_id);
        }
        Expr::Path(path) => {
            if let Some(base) = &mut path.base {
                rebind_expr(base, file_id);
            }
            rebind_ident(&mut path.type_name, file_id);
            rebind_ident(&mut path.variant, file_id);
            rebind_span(&mut path.span, file_id);
        }
        Expr::SelfExpr(self_expr) => rebind_span(&mut self_expr.span, file_id),
        Expr::Comptime(block) => {
            rebind_expr(&mut block.expr, file_id);
            rebind_span(&mut block.span, file_id);
        }
        Expr::Checked(block) => {
            rebind_expr(&mut block.expr, file_id);
            rebind_span(&mut block.span, file_id);
        }
        Expr::TypeLit(literal) => {
            rebind_type(&mut literal.type_expr, file_id);
            rebind_span(&mut literal.span, file_id);
        }
        Expr::Error(span) => rebind_span(span, file_id),
    }
}

fn rebind_let_pattern(pattern: &mut LetPattern, file_id: FileId) {
    match pattern {
        LetPattern::Ident(ident) => rebind_ident(ident, file_id),
        LetPattern::Wildcard(span) => rebind_span(span, file_id),
    }
}

fn rebind_statement(statement: &mut Statement, file_id: FileId) {
    match statement {
        Statement::Let(binding) => {
            if let Some(directives) = binding.directives.as_deref_mut() {
                rebind_directives(directives, file_id);
            }
            rebind_let_pattern(&mut binding.pattern, file_id);
            if let Some(ty) = &mut binding.ty {
                rebind_type(ty, file_id);
            }
            rebind_expr(&mut binding.init, file_id);
            rebind_span(&mut binding.span, file_id);
        }
        Statement::Assign(assignment) => {
            match &mut assignment.target {
                AssignTarget::Var(ident) => rebind_ident(ident, file_id),
                AssignTarget::Field(field) => {
                    rebind_expr(&mut field.base, file_id);
                    rebind_ident(&mut field.field, file_id);
                    rebind_span(&mut field.span, file_id);
                }
                AssignTarget::Index(index) => {
                    rebind_expr(&mut index.base, file_id);
                    rebind_expr(&mut index.index, file_id);
                    rebind_span(&mut index.span, file_id);
                }
                AssignTarget::Method(expr) => rebind_expr(expr, file_id),
            }
            rebind_expr(&mut assignment.value, file_id);
            rebind_span(&mut assignment.span, file_id);
        }
        Statement::Expr(expr) => rebind_expr(expr, file_id),
    }
}

// Display implementations for AST pretty-printing

impl fmt::Display for Ast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in &self.items {
            match item {
                Item::Function(func) => fmt_function(f, func, 0)?,
                Item::Struct(s) => fmt_struct(f, s, 0)?,
                Item::Enum(e) => fmt_enum(f, e, 0)?,
                Item::DropFn(drop_fn) => fmt_drop_fn(f, drop_fn, 0)?,
                Item::Extern(extern_block) => fmt_extern(f, extern_block, 0)?,
                Item::Const(c) => fmt_const(f, c, 0)?,
                Item::Error(span) => writeln!(f, "Error({:?})", span)?,
            }
        }
        Ok(())
    }
}

fn indent(f: &mut fmt::Formatter<'_>, level: usize) -> fmt::Result {
    for _ in 0..level {
        write!(f, "  ")?;
    }
    Ok(())
}

fn fmt_struct(f: &mut fmt::Formatter<'_>, s: &StructDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    for directive in &s.directives {
        write!(f, "@sym:{} ", directive.name.name.into_usize())?;
    }
    if s.is_linear {
        write!(f, "linear ")?;
    }
    writeln!(f, "Struct sym:{}", s.name.name.into_usize())?;
    for field in &s.fields {
        indent(f, level + 1)?;
        writeln!(
            f,
            "Field sym:{} : {}",
            field.name.name.into_usize(),
            field.ty
        )?;
    }
    for method in &s.methods {
        fmt_method(f, method, level + 1)?;
    }
    Ok(())
}

fn fmt_enum(f: &mut fmt::Formatter<'_>, e: &EnumDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    for directive in &e.directives {
        write!(f, "@sym:{} ", directive.name.name.into_usize())?;
    }
    writeln!(f, "Enum sym:{}", e.name.name.into_usize())?;
    for variant in &e.variants {
        indent(f, level + 1)?;
        if variant.payload.is_empty() {
            writeln!(f, "Variant sym:{}", variant.name.name.into_usize())?;
        } else {
            writeln!(
                f,
                "Variant sym:{} payload:{}",
                variant.name.name.into_usize(),
                variant.payload.len()
            )?;
        }
    }
    Ok(())
}

fn fmt_extern(f: &mut fmt::Formatter<'_>, block: &ExternBlock, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(f, "Extern \"{}\"", block.abi)?;
    for foreign in &block.fns {
        indent(f, level + 1)?;
        writeln!(f, "ExternFn sym:{}", foreign.name.name.into_usize())?;
    }
    Ok(())
}

fn fmt_const(f: &mut fmt::Formatter<'_>, c: &ConstDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    for directive in &c.directives {
        write!(f, "@sym:{} ", directive.name.name.into_usize())?;
    }
    if c.visibility == Visibility::Public {
        write!(f, "pub ")?;
    }
    write!(f, "Const sym:{}", c.name.name.into_usize())?;
    if let Some(ref ty) = c.ty {
        write!(f, ": {}", ty)?;
    }
    writeln!(f)?;
    fmt_expr(f, &c.init, level + 1)?;
    Ok(())
}

fn fmt_drop_fn(f: &mut fmt::Formatter<'_>, drop_fn: &DropFn, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(
        f,
        "DropFn sym:{}(self)",
        drop_fn.type_name.name.into_usize()
    )?;
    fmt_expr(f, &drop_fn.body, level + 1)?;
    Ok(())
}

fn fmt_method(f: &mut fmt::Formatter<'_>, method: &Method, level: usize) -> fmt::Result {
    indent(f, level)?;
    write!(f, "Method sym:{}", method.name.name.into_usize())?;
    write!(f, "(")?;
    if let Some(receiver) = &method.receiver {
        match receiver.mode {
            ParamMode::Inout => write!(f, "inout ")?,
            ParamMode::Borrow => write!(f, "borrow ")?,
            _ => {}
        }
        write!(f, "self")?;
        if !method.params.is_empty() {
            write!(f, ", ")?;
        }
    }
    for (i, param) in method.params.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        fmt_param(f, param)?;
    }
    write!(f, ")")?;
    if let Some(ref ret) = method.return_type {
        write!(f, " -> {}", ret)?;
    }
    writeln!(f)?;
    fmt_expr(f, &method.body, level + 1)?;
    Ok(())
}

fn fmt_param(f: &mut fmt::Formatter<'_>, param: &Param) -> fmt::Result {
    match param.mode {
        ParamMode::Inout => write!(f, "inout ")?,
        ParamMode::Borrow => write!(f, "borrow ")?,
        ParamMode::Comptime => write!(f, "comptime ")?,
        ParamMode::Normal => {}
    }
    write!(f, "sym:{}: {}", param.name.name.into_usize(), param.ty)
}

fn fmt_call_arg(f: &mut fmt::Formatter<'_>, arg: &CallArg, level: usize) -> fmt::Result {
    match arg.mode {
        ArgMode::Inout => {
            indent(f, level)?;
            writeln!(f, "inout:")?;
            fmt_expr(f, &arg.expr, level + 1)
        }
        ArgMode::Borrow => {
            indent(f, level)?;
            writeln!(f, "borrow:")?;
            fmt_expr(f, &arg.expr, level + 1)
        }
        ArgMode::Normal => fmt_expr(f, &arg.expr, level),
    }
}

fn fmt_function(f: &mut fmt::Formatter<'_>, func: &Function, level: usize) -> fmt::Result {
    indent(f, level)?;
    if let Some(abi) = &func.export_abi {
        write!(f, "pub extern \"{abi}\" ")?;
    }
    if func.is_unchecked {
        write!(f, "unchecked ")?;
    }
    write!(f, "Function sym:{}", func.name.name.into_usize())?;
    if !func.params.is_empty() {
        write!(f, "(")?;
        for (i, param) in func.params.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            fmt_param(f, param)?;
        }
        write!(f, ")")?;
    }
    if let Some(ref ret) = func.return_type {
        write!(f, " -> {}", ret)?;
    }
    writeln!(f)?;
    fmt_expr(f, &func.body, level + 1)?;
    Ok(())
}

fn fmt_expr(f: &mut fmt::Formatter<'_>, expr: &Expr, level: usize) -> fmt::Result {
    indent(f, level)?;
    match expr {
        Expr::Int(lit) => writeln!(f, "Int({})", lit.value),
        Expr::Float(lit) => writeln!(f, "Float(sym:{})", lit.value.into_usize()),
        Expr::String(lit) => writeln!(f, "String(sym:{})", lit.value.into_usize()),
        Expr::Bool(lit) => writeln!(f, "Bool({})", lit.value),
        Expr::Unit(_) => writeln!(f, "Unit"),
        Expr::Ident(ident) => writeln!(f, "Ident(sym:{})", ident.name.into_usize()),
        Expr::Binary(bin) => {
            writeln!(f, "Binary {:?}", bin.op)?;
            fmt_expr(f, &bin.left, level + 1)?;
            fmt_expr(f, &bin.right, level + 1)
        }
        Expr::Unary(un) => {
            writeln!(f, "Unary {:?}", un.op)?;
            fmt_expr(f, &un.operand, level + 1)
        }
        Expr::Paren(paren) => {
            writeln!(f, "Paren")?;
            fmt_expr(f, &paren.inner, level + 1)
        }
        Expr::Block(block) => {
            writeln!(f, "Block")?;
            for stmt in &block.statements {
                fmt_stmt(f, stmt, level + 1)?;
            }
            fmt_expr(f, &block.expr, level + 1)
        }
        Expr::If(if_expr) => {
            writeln!(f, "If")?;
            indent(f, level + 1)?;
            writeln!(f, "Cond:")?;
            fmt_expr(f, &if_expr.cond, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Then:")?;
            fmt_block_expr(f, &if_expr.then_block, level + 2)?;
            if let Some(ref else_block) = if_expr.else_block {
                indent(f, level + 1)?;
                writeln!(f, "Else:")?;
                fmt_block_expr(f, else_block, level + 2)?;
            }
            Ok(())
        }
        Expr::Match(match_expr) => {
            writeln!(f, "Match")?;
            indent(f, level + 1)?;
            writeln!(f, "Scrutinee:")?;
            fmt_expr(f, &match_expr.scrutinee, level + 2)?;
            for arm in &match_expr.arms {
                indent(f, level + 1)?;
                writeln!(f, "Arm {:?} =>", arm.pattern)?;
                fmt_expr(f, &arm.body, level + 2)?;
            }
            Ok(())
        }
        Expr::While(while_expr) => {
            writeln!(f, "While")?;
            indent(f, level + 1)?;
            writeln!(f, "Cond:")?;
            fmt_expr(f, &while_expr.cond, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Body:")?;
            fmt_block_expr(f, &while_expr.body, level + 2)
        }
        Expr::Loop(loop_expr) => {
            writeln!(f, "Loop")?;
            fmt_block_expr(f, &loop_expr.body, level + 1)
        }
        Expr::For(for_expr) => {
            writeln!(f, "For")?;
            indent(f, level + 1)?;
            match &for_expr.binder {
                LetPattern::Ident(ident) => writeln!(f, "Binder sym:{}", ident.name.into_usize())?,
                LetPattern::Wildcard(_) => writeln!(f, "Binder _")?,
            }
            indent(f, level + 1)?;
            writeln!(f, "Iterable:")?;
            fmt_expr(f, &for_expr.iterable, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Body:")?;
            fmt_block_expr(f, &for_expr.body, level + 2)
        }
        Expr::Call(call) => {
            writeln!(f, "Call sym:{}", call.name.name.into_usize())?;
            for arg in &call.args {
                fmt_call_arg(f, arg, level + 1)?;
            }
            Ok(())
        }
        Expr::IntrinsicCall(intrinsic) => {
            writeln!(f, "Intrinsic @sym:{}", intrinsic.name.name.into_usize())?;
            for arg in &intrinsic.args {
                match arg {
                    IntrinsicArg::Expr(expr) => fmt_expr(f, expr, level + 1)?,
                    IntrinsicArg::Type(ty) => {
                        indent(f, level + 1)?;
                        writeln!(f, "Type {:?}", ty)?;
                    }
                }
            }
            Ok(())
        }
        Expr::Break(brk) => {
            if let Some(ref value) = brk.value {
                writeln!(f, "Break")?;
                fmt_expr(f, value, level + 1)
            } else {
                writeln!(f, "Break")
            }
        }
        Expr::Continue(_) => writeln!(f, "Continue"),
        Expr::Return(ret) => {
            if let Some(ref value) = ret.value {
                writeln!(f, "Return")?;
                fmt_expr(f, value, level + 1)
            } else {
                writeln!(f, "Return (unit)")
            }
        }
        Expr::Yield(yield_expr) => {
            writeln!(f, "Yield")?;
            fmt_expr(f, &yield_expr.value, level + 1)
        }
        Expr::StructLit(lit) => {
            writeln!(f, "StructLit sym:{}", lit.name.name.into_usize())?;
            for field in &lit.fields {
                indent(f, level + 1)?;
                writeln!(f, "sym:{} =", field.name.name.into_usize())?;
                fmt_expr(f, &field.value, level + 2)?;
            }
            Ok(())
        }
        Expr::Field(field) => {
            writeln!(f, "Field .sym:{}", field.field.name.into_usize())?;
            fmt_expr(f, &field.base, level + 1)
        }
        Expr::Try(try_expr) => {
            writeln!(f, "Try ?")?;
            fmt_expr(f, &try_expr.operand, level + 1)
        }
        Expr::MethodCall(method_call) => {
            writeln!(
                f,
                "MethodCall .sym:{}",
                method_call.method.name.into_usize()
            )?;
            indent(f, level + 1)?;
            writeln!(f, "Receiver:")?;
            fmt_expr(f, &method_call.receiver, level + 2)?;
            if !method_call.args.is_empty() {
                indent(f, level + 1)?;
                writeln!(f, "Args:")?;
                for arg in &method_call.args {
                    fmt_call_arg(f, arg, level + 2)?;
                }
            }
            Ok(())
        }
        Expr::ArrayLit(array) => {
            match &array.repeat {
                Some(count) => writeln!(f, "ArrayLit (repeat; count={count})")?,
                None => writeln!(f, "ArrayLit")?,
            }
            for elem in &array.elements {
                fmt_expr(f, elem, level + 1)?;
            }
            Ok(())
        }
        Expr::Index(index) => {
            writeln!(f, "Index")?;
            indent(f, level + 1)?;
            writeln!(f, "Base:")?;
            fmt_expr(f, &index.base, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Index:")?;
            fmt_expr(f, &index.index, level + 2)
        }
        Expr::Path(path) => writeln!(
            f,
            "Path sym:{}::sym:{}",
            path.type_name.name.into_usize(),
            path.variant.name.into_usize()
        ),
        Expr::SelfExpr(_) => {
            writeln!(f, "SelfExpr")
        }
        Expr::Comptime(comptime) => {
            writeln!(f, "Comptime")?;
            fmt_expr(f, &comptime.expr, level + 1)
        }
        Expr::Checked(checked) => {
            writeln!(f, "Checked")?;
            fmt_expr(f, &checked.expr, level + 1)
        }
        Expr::TypeLit(type_lit) => {
            writeln!(f, "TypeLit({})", type_lit.type_expr)
        }
        Expr::Error(span) => {
            writeln!(f, "Error({:?})", span)
        }
    }
}

fn fmt_block_expr(f: &mut fmt::Formatter<'_>, block: &BlockExpr, level: usize) -> fmt::Result {
    for stmt in &block.statements {
        fmt_stmt(f, stmt, level)?;
    }
    fmt_expr(f, &block.expr, level)
}

fn fmt_stmt(f: &mut fmt::Formatter<'_>, stmt: &Statement, level: usize) -> fmt::Result {
    indent(f, level)?;
    match stmt {
        Statement::Let(let_stmt) => {
            write!(f, "Let")?;
            if let_stmt.is_mut {
                write!(f, " mut")?;
            }
            match &let_stmt.pattern {
                LetPattern::Ident(ident) => write!(f, " sym:{}", ident.name.into_usize())?,
                LetPattern::Wildcard(_) => write!(f, " _")?,
            }
            if let Some(ref ty) = let_stmt.ty {
                write!(f, ": {}", ty)?;
            }
            writeln!(f)?;
            fmt_expr(f, &let_stmt.init, level + 1)
        }
        Statement::Assign(assign) => {
            // A plain assignment prints exactly as before; a compound one names
            // its operator so the two forms are distinguishable (RUE-1043).
            let op = match assign.op {
                Some(op) => format!("{} ", op.spelling()),
                None => String::new(),
            };
            match &assign.target {
                AssignTarget::Var(ident) => {
                    writeln!(f, "Assign {}sym:{}", op, ident.name.into_usize())?
                }
                AssignTarget::Field(field) => {
                    writeln!(
                        f,
                        "Assign {}field .sym:{}",
                        op,
                        field.field.name.into_usize()
                    )?;
                    fmt_expr(f, &field.base, level + 1)?;
                }
                AssignTarget::Index(index) => {
                    writeln!(f, "Assign {}index", op)?;
                    indent(f, level + 1)?;
                    writeln!(f, "Base:")?;
                    fmt_expr(f, &index.base, level + 2)?;
                    indent(f, level + 1)?;
                    writeln!(f, "Index:")?;
                    fmt_expr(f, &index.index, level + 2)?;
                }
                AssignTarget::Method(expr) => {
                    writeln!(f, "Assign {}method", op)?;
                    fmt_expr(f, expr, level + 1)?;
                }
            }
            fmt_expr(f, &assign.value, level + 1)
        }
        Statement::Expr(expr) => {
            writeln!(f, "ExprStmt")?;
            fmt_expr(f, expr, level + 1)
        }
    }
}

#[cfg(test)]
mod size_guards {
    use super::*;

    /// RUE-1836. The AST is retained for the life of a module revision
    /// (`ParsedModule` holds `ast: Arc<Ast>`, and `retained_charge()` bills its
    /// bytes as session memory), and during parsing every `PResult<Expr>` return
    /// and `Vec` push memcpys these values. Both costs scale with the *largest*
    /// variant, so the common nodes pay for the rare shapes.
    ///
    /// Upper bounds rather than equalities: shrinking further is always welcome,
    /// and these are 64-bit figures. Nothing else in the repo guarded AST sizes,
    /// so any new variant could silently widen every node — these are the tests
    /// the issue asked for, beside `rue-span`'s `test_span_size`.
    #[test]
    fn ast_nodes_stay_small() {
        // Was 128: `Expr::If` inlined a 120-byte `IfExpr` holding both blocks.
        assert!(
            size_of::<Expr>() <= 96,
            "Expr grew to {} bytes; the cap is StructLitExpr at {}",
            size_of::<Expr>(),
            size_of::<StructLitExpr>(),
        );
        // Was 192, driven by LetStatement.
        assert!(
            size_of::<Statement>() <= 96,
            "Statement grew to {} bytes",
            size_of::<Statement>(),
        );
        // Was 176: an inline `Directives` SmallVec plus an inline `TypeExpr`.
        assert!(
            size_of::<LetStatement>() <= 56,
            "LetStatement grew to {} bytes",
            size_of::<LetStatement>(),
        );
        // Was 144; inlines one `Expr`, so it tracks `Expr` above.
        assert!(
            size_of::<CallArg>() <= 112,
            "CallArg grew to {} bytes",
            size_of::<CallArg>(),
        );
    }

    /// The boxes above only pay off while the nodes they point at stay off the
    /// hot inline path. If `IfExpr` were ever inlined back into `Expr`, the
    /// bound above would fail — but so would the intent, so state it directly.
    #[test]
    fn oversized_nodes_stay_behind_a_pointer() {
        assert!(size_of::<IfExpr>() > size_of::<Expr>());
        assert_eq!(size_of::<Option<Box<Directives>>>(), size_of::<usize>());
        assert_eq!(size_of::<Option<Box<TypeExpr>>>(), size_of::<usize>());
    }
}
