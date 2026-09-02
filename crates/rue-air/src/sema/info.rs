//! Information types for functions, methods, and constants.
//!
//! These types store metadata about declarations gathered during the first
//! phase of semantic analysis. They are used to resolve function calls,
//! method calls, and constant references during function body analysis.

use lasso::Spur;
use rue_span::{FileId, Span};

use crate::param_arena::ParamRange;
use crate::types::{StructId, Type};

/// Information about a function.
#[derive(Debug, Clone, Copy)]
pub struct FunctionInfo {
    /// Parameter data (names, types, modes, comptime flags) stored in arena.
    /// Access via `arena.names(params)`, `arena.types(params)`, etc.
    pub params: ParamRange,
    /// Return type
    pub return_type: Type,
    /// Declaration-local structured return syntax used by generic
    /// specialization. The reference belongs to the same RIR as
    /// `declaration`; it is never transferred across an RIR owner.
    pub return_type_syntax: rue_rir::RirTypeSyntaxRef,
    /// Whether the source return annotation is literally `type`.
    ///
    /// `return_type` cannot answer this because dependent return syntax such as
    /// `T` also uses `COMPTIME_TYPE` until specialization.  Carrying the kind
    /// bit keeps call-site classification independent of any rendered or
    /// source-impossible type-syntax symbol.
    pub returns_type: bool,
    /// RIR body ref for generic specialization
    pub body: rue_rir::InstRef,
    /// Owning source declaration. Variable payload descriptors remain attached
    /// to this instruction and are borrowed from the RIR on demand.
    pub declaration: rue_rir::InstRef,
    /// Span of the function declaration
    pub span: Span,
    /// Whether this function has any comptime parameters (type or value) and
    /// therefore requires per-call-site specialization (RUE-166)
    pub is_generic: bool,
    /// Whether this function is public (visible outside its directory)
    pub is_pub: bool,
    /// Whether this function carries the `unchecked` modifier. Calling it
    /// requires a `checked` block at the call site (spec 9.1:1).
    pub is_unchecked: bool,
    /// Whether this is a foreign `extern "C"` declaration (ADR-0064 C FFI): a
    /// body-less import with no CFG. Calling it requires the `c_ffi` preview and
    /// a `checked` block; codegen emits an undefined linker symbol rather than a
    /// definition.
    pub is_extern: bool,
    /// Whether this is a Rue-to-C export (`pub extern "C" fn`, ADR-0064 P4): an
    /// ordinary Rue function body (analyzed, with a CFG, code-generated) that is
    /// *also* exposed to C callers under its unmangled source name via a C-ABI
    /// callee thunk. Unlike `is_extern`, an export is a reachability root and
    /// requires codegen to emit its entry thunk.
    pub is_c_export: bool,
    /// Whether `@allow(unused_function)` was applied to this function.
    pub allow_unused_function: bool,
    /// Whether `@allow(unused_variable)` was applied to this function.
    pub allow_unused_variable: bool,
    /// Whether `@allow(unreachable_code)` was applied to this function.
    pub allow_unreachable_code: bool,
    /// File ID this function was declared in (for visibility checking)
    pub file_id: FileId,
}

impl FunctionInfo {}

/// Information about a method in an impl block.
///
/// Note: Captured comptime values for anonymous struct methods are stored at
/// the struct level on the body host, not per-method. This ensures
/// that different instantiations with different captured values create different types.
#[derive(Debug, Clone, Copy)]
pub struct MethodInfo {
    /// The struct type this method belongs to
    pub struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// The receiver's passing mode when `has_self` is true (`Normal`
    /// by-value, `Borrow`, or `Inout`; RUE-15). Determines how the receiver
    /// is passed at call sites (by value vs. by reference / autoref).
    pub self_mode: rue_rir::RirParamMode,
    /// Whether the receiver is declared `mut self` (by-value receiver that
    /// binds mutably in the method body). Body-local only: call sites and
    /// structural identity (`AnonMethodSig`) deliberately ignore it.
    pub self_is_mut: bool,
    /// Parameter data (names, types, modes, comptime flags) stored in arena.
    /// Access via `arena.names(params)`, `arena.types(params)`, etc.
    /// Note: This excludes `self` if present - only explicit parameters.
    pub params: ParamRange,
    /// Return type
    pub return_type: Type,
    /// The RIR instruction ref for the method body
    pub body: rue_rir::InstRef,
    /// Span of the method declaration
    pub span: Span,
    /// Whether the result position is `-> borrow T` (ADR-0062) or `-> inout T`
    /// (RUE-1016): the method is a place-returning accessor whose calls inline
    /// to guards plus the yielded receiver projection. `return_type` holds T.
    pub returns_borrow: bool,
    pub returns_inout: bool,
}

/// Signature-only callable metadata consumed at a call site. Imported
/// callables deliberately have no producer-local RIR handles or spans.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FunctionCallInfo {
    pub params: ParamRange,
    pub return_type: Type,
    pub returns_type: bool,
    pub is_generic: bool,
    pub is_pub: bool,
    pub is_unchecked: bool,
    pub is_extern: bool,
    pub file_id: FileId,
}

impl FunctionCallInfo {
    pub(crate) fn from_body(info: FunctionInfo) -> Self {
        Self {
            params: info.params,
            return_type: info.return_type,
            returns_type: info.returns_type,
            is_generic: info.is_generic,
            is_pub: info.is_pub,
            is_unchecked: info.is_unchecked,
            is_extern: info.is_extern,
            file_id: info.file_id,
        }
    }
}

/// Signature-only method metadata consumed for dispatch. The producer's method
/// body and declaration span remain exclusively in [`MethodInfo`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct MethodCallInfo {
    pub struct_type: Type,
    pub has_self: bool,
    pub self_mode: rue_rir::RirParamMode,
    pub params: ParamRange,
    pub return_type: Type,
    /// Whether the method is a `-> borrow T` accessor (ADR-0062). Accessor
    /// calls do not dispatch as ordinary calls: they inline the accessor
    /// body at the call site via the dedicated accessor-body fact.
    pub returns_borrow: bool,
    pub returns_inout: bool,
}

impl MethodCallInfo {
    pub(crate) fn from_body(info: MethodInfo) -> Self {
        Self {
            struct_type: info.struct_type,
            has_self: info.has_self,
            self_mode: info.self_mode,
            params: info.params,
            return_type: info.return_type,
            returns_borrow: info.returns_borrow,
            returns_inout: info.returns_inout,
        }
    }
}

/// Method signature for anonymous struct structural equality comparison.
///
/// This captures only the parts of a method that affect structural equality:
/// method name, receiver presence and mode, explicit parameter types/modes,
/// comptime flags, and return type. Method bodies do NOT affect structural
/// equality - only signatures matter.
///
/// Type symbols are stored as Spur (interned strings) rather than resolved Types
/// because at comparison time, `Self` hasn't been resolved to a concrete StructId yet.
/// Two methods using `Self` in the same positions are considered structurally equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnonMethodType {
    SelfType,
    Concrete(Type),
    /// Unsupported syntax is retained as a deterministic fail-closed shape;
    /// equal spelling may match, but it cannot alias a differently spelled
    /// semantic type by accident.
    Syntax(std::sync::Arc<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonMethodSig {
    /// Method name
    pub name: Spur,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Receiver passing mode. Meaningful when `has_self` is true; associated
    /// functions carry Normal.
    pub self_mode: rue_rir::RirParamMode,
    /// Whether this signature returns a shared second-class place.
    pub returns_borrow: bool,
    /// Whether this signature returns an exclusive second-class place.
    pub returns_inout: bool,
    /// Parameter type symbols (excluding self parameter)
    pub param_types: Vec<AnonMethodType>,
    /// Explicit parameter passing modes, parallel to `param_types`.
    pub param_modes: Vec<rue_rir::RirParamMode>,
    /// Explicit parameter comptime flags, parallel to `param_types`.
    pub param_comptime: Vec<bool>,
    /// Return type symbol
    pub return_type: AnonMethodType,
}

/// Information about a constant declaration.
///
/// Constants are compile-time values. In the module system, they're primarily
/// used for re-exports:
/// ```rue
/// pub const strings = @import("utils/strings.rue");
/// pub const helper = @import("utils/internal.rue").helper;
/// ```
#[derive(Debug, Clone)]
pub struct ConstInfo {
    /// Whether this constant is public
    pub is_pub: bool,
    /// The type of the constant value (e.g., Type::Module for imports)
    pub ty: Type,
    /// The RIR instruction ref for the initializer
    /// The compile-time value of the initializer, evaluated once during
    /// declaration gathering. Module bindings store `ConstValue::Type` of
    /// their module type; value constants store the evaluated value, which
    /// use sites materialize directly (no re-analysis of the initializer).
    pub value: crate::sema::ConstValue,
    /// Span of the const declaration
    pub span: Span,
}

/// The declaration facts of an `interface` shell (spec 6.7), read by
/// conformance verification. `parents` are the interfaces it refines
/// (spec 6.7:7); `assoc_requirements` are its `const Name: type;`
/// requirements; `method_requirements` name its method and
/// associated-function requirements, whose signatures are the shell's own
/// methods.
#[derive(Debug, Clone)]
pub(crate) struct InterfaceFacts {
    pub name: std::sync::Arc<str>,
    pub parents: Vec<StructId>,
    pub assoc_requirements: Vec<std::sync::Arc<str>>,
    pub method_requirements: Vec<std::sync::Arc<str>>,
}

/// One conformance assertion for a subject type (spec 6.7:9): the asserted
/// interface and, when the host analyzes against a concrete snapshot, the
/// span of the assertion itself (a struct header's `is` list or a
/// freestanding `Type is I;`), where a failed verification is reported.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConformanceAssertion {
    pub interface: StructId,
    pub span: Option<Span>,
}

/// An interface requirement's signature after substituting a subject type
/// for `Self` and the subject's associated types for the interface's
/// associated-constant names (spec 6.7:10), in the vocabulary a subject's
/// inherent member is compared against.
#[derive(Debug, Clone)]
pub(crate) struct RequirementSignature {
    pub has_self: bool,
    pub self_mode: rue_rir::RirParamMode,
    pub params: Vec<(std::sync::Arc<str>, rue_rir::RirParamMode, Type)>,
    pub result: Type,
}
