//! Error types for the Rue compiler.
//!
//! This crate provides the error infrastructure used throughout the compilation
//! pipeline. Errors carry source location information for diagnostic rendering.
//!
//! # Diagnostic System
//!
//! Errors and warnings can include rich diagnostic information:
//! - **Labels**: Secondary spans pointing to related code locations
//! - **Notes**: Informational context about the error/warning
//! - **Helps**: Actionable suggestions for fixing the issue
//!
//! Example:
//! ```ignore
//! CompileError::new(ErrorKind::TypeMismatch { ... }, span)
//!     .with_label("expected because of this", other_span)
//!     .with_help("consider using a type conversion")
//! ```

pub mod ice;

use rue_span::Span;

/// The rue compiler version.
///
/// Single source of truth: the CLI's `--version` banner and ICE reports
/// (via the `ice!`/`ice_error!` macros) both read this constant. Buck2
/// builds don't set `CARGO_PKG_VERSION`, so it can't come from the
/// environment.
pub const VERSION: &str = "0.1.0";
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use thiserror::Error;

// ============================================================================
// Error Codes
// ============================================================================
//
// Every error kind has a unique, stable error code for searchability.
// Codes are assigned by category and must never change once assigned.
// See issue rue-0c9y for the design rationale.

/// A unique error code for each error type.
///
/// Error codes are formatted as `E` followed by a 4-digit zero-padded number
/// (e.g., `E0001`, `E0042`). They are assigned by category:
///
/// - **E0001-E0099**: Lexer errors (tokenization)
/// - **E0100-E0199**: Parser errors (syntax)
/// - **E0200-E0399**: Semantic errors (types, names, scopes)
/// - **E0400-E0499**: Struct/enum errors
/// - **E0500-E0599**: Control flow errors
/// - **E0600-E0699**: Match errors
/// - **E0700-E0799**: Intrinsic errors
/// - **E0800-E0899**: Literal/operator errors
/// - **E0900-E0999**: Array errors
/// - **E1000-E1099**: Linker/target errors
/// - **E1100-E1199**: Preview feature errors
/// - **E1200-E1299**: Comptime errors
/// - **E1300-E1399**: Unchecked-code errors (raw pointers, `checked` blocks)
/// - **E1400-E1499**: Compiler-input errors
/// - **E9000-E9999**: Internal compiler errors
///
/// Once assigned, error codes must never change to maintain stability for
/// documentation, search engines, and user bookmarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(pub u16);

/// Maximum supported syntactic nesting depth.
///
/// The parser (recursive-descent through bracketed constructs) and AstGen
/// (recursive AST→RIR lowering) both bound their recursion by this value and
/// emit [`ErrorCode::NESTING_LIMIT_EXCEEDED`] (E0482) instead of overflowing
/// the stack on pathologically nested input (RUE-42). Chosen generously so
/// real code never hits it, but low enough that the guarded recursion stays
/// well within the stack the parser runs on. Published as the nesting-depth
/// limit in spec C.6:3; the diagnosable-failure requirement is spec C.1:2.
pub const MAX_NESTING_DEPTH: usize = 256;

impl ErrorCode {
    // ========================================================================
    // Lexer errors (E0001-E0099)
    // ========================================================================
    pub const UNEXPECTED_CHARACTER: Self = Self(1);
    pub const INVALID_INTEGER: Self = Self(2);
    pub const INVALID_STRING_ESCAPE: Self = Self(3);
    pub const UNTERMINATED_STRING: Self = Self(4);
    // E0005 (UNSUPPORTED_INTEGER_BASE) is retired: 0x/0o/0b literals are now
    // valid Rue syntax (RUE-177). Do not reuse the code.
    pub const UPPERCASE_BASE_PREFIX: Self = Self(6);
    pub const EMPTY_BASED_LITERAL: Self = Self(7);
    pub const INVALID_DIGIT_FOR_BASE: Self = Self(8);
    pub const MALFORMED_BYTE_LITERAL: Self = Self(9);
    /// The per-file lexer diagnostic budget was exceeded. The detailed
    /// diagnostics before this summary remain available.
    pub const LEXER_DIAGNOSTICS_OMITTED: Self = Self(10);
    /// A floating-point literal written with a leading dot (`.5`) or a
    /// trailing dot (`5.`); ADR-0065 §3 requires `0.5` / `5.0`. Sits in the
    /// lexical band even though the trailing-dot form is diagnosed by the
    /// parser — see [`ErrorKind::MalformedFloatLiteral`] for why that half
    /// cannot be decided from the lexeme alone. (RUE-1068)
    pub const MALFORMED_FLOAT_LITERAL: Self = Self(11);

    // ========================================================================
    // Parser errors (E0100-E0199)
    // ========================================================================
    pub const UNEXPECTED_TOKEN: Self = Self(100);
    // E0101 was UNEXPECTED_EOF, deleted as never-produced: the parser reports
    // end-of-file as UnexpectedToken { found: "end of file" }. Keep the
    // number retired rather than reusing it.
    pub const PARSE_ERROR: Self = Self(102);
    /// The per-file parser recovery diagnostic budget was exceeded. The
    /// detailed diagnostics before this summary remain available.
    pub const PARSER_DIAGNOSTICS_OMITTED: Self = Self(103);

    // ========================================================================
    // Semantic errors (E0200-E0399)
    // ========================================================================
    pub const NO_MAIN_FUNCTION: Self = Self(200);
    pub const UNDEFINED_VARIABLE: Self = Self(201);
    pub const UNDEFINED_FUNCTION: Self = Self(202);
    pub const ASSIGN_TO_IMMUTABLE: Self = Self(203);
    pub const UNKNOWN_TYPE: Self = Self(204);
    pub const USE_AFTER_MOVE: Self = Self(205);
    pub const TYPE_MISMATCH: Self = Self(206);
    pub const WRONG_ARGUMENT_COUNT: Self = Self(207);
    pub const MOVE_WHILE_CALL_LOANED: Self = Self(208);
    pub const UNEXPECTED_CALL_ARGUMENT_MODE: Self = Self(209);
    /// Whole-value assignment to a second-class `inout str` view. The view
    /// grants exclusive access to the caller's bytes; it is not a first-class
    /// string header that may be rebound (RUE-641).
    pub const STR_VIEW_REASSIGNMENT: Self = Self(210);
    /// The executable entry point has parameters or a return type other than
    /// `i32` or `()` (spec 6.1:8, RUE-778). The runtime calls `main` with no
    /// arguments and consumes either its status code or the unit value, so a
    /// different source signature would violate the entry ABI.
    pub const INVALID_MAIN_SIGNATURE: Self = Self(211);
    // E0250-E0261 form the borrow-accessor block (ADR-0062, RUE-662). The
    // ownership/borrow family's E04xx band is at its ceiling (E0499), so
    // accessor diagnostics live here in the semantic band instead.
    /// An accessor result (`v.get_ref(i)`) was returned from the enclosing
    /// function. The result is a second-class borrowed place scoped to the
    /// enclosing full expression (ADR-0062); returning it would let the loan
    /// outlive the receiver access that justifies it.
    pub const ACCESSOR_RESULT_RETURNED: Self = Self(250);
    /// An accessor result was stored — assigned to a variable, field, or
    /// element. The result is a second-class borrowed place (ADR-0062); a
    /// stored copy would be a stored borrow, which Rue does not have.
    pub const ACCESSOR_RESULT_STORED: Self = Self(251);
    /// An accessor result was bound by a plain `let`. The result's extent is
    /// the enclosing full expression (ADR-0062), so a binding would outlive
    /// the loan. Use the result directly within one expression instead.
    pub const ACCESSOR_RESULT_BOUND: Self = Self(252);
    /// An accessor result was captured into an aggregate (struct literal or
    /// array literal). The result is a second-class borrowed place
    /// (ADR-0062); an aggregate member holding it would be a stored borrow.
    pub const ACCESSOR_RESULT_CAPTURED: Self = Self(253);
    /// An accessor body's non-diverging control flow does not end in the
    /// single trailing `yield` (ADR-0062 phase 1): the final statement is not
    /// a `yield`, a `yield` appears before the end, or the body contains a
    /// `return`/`?` exit. Guards before the yield may only diverge (trap,
    /// `@panic`) or fall through.
    pub const ACCESSOR_BODY_MISSING_YIELD: Self = Self(254);
    /// The operand of an accessor's `yield` is not a place rooted at the
    /// receiver parameter (`self`). An accessor hands out a projection of its
    /// receiver (ADR-0062); yielding a local, temporary, or unrelated place
    /// would dangle once the accessor's frame is gone.
    pub const ACCESSOR_YIELD_NOT_RECEIVER_ROOTED: Self = Self(255);
    /// A `yield` expression appears outside the body of a `-> borrow T`
    /// accessor. `yield` is the accessor body's exit form (ADR-0062).
    pub const YIELD_OUTSIDE_ACCESSOR: Self = Self(256);
    /// A `-> borrow T` result on a declaration that is not a `borrow self`
    /// method: a free function, an associated function, or a method with a
    /// by-value or `mut self` receiver. Phase 1 accessors are read-only
    /// projections of a shared receiver borrow (ADR-0062); `inout self`
    /// accessors are the phase-2 follow-up (RUE-1016).
    pub const ACCESSOR_REQUIRES_BORROW_SELF: Self = Self(257);
    /// A value with drop glue was read out of an accessor result by value.
    /// The result is a borrowed place, not an owner (ADR-0062); copying a
    /// drop-glue value out of it would mint an aliasing second owner — the
    /// same double-free the E0711 gate closes (RUE-651). Only trivially
    /// droppable element values may be read out by value.
    pub const ACCESSOR_RESULT_MOVED: Self = Self(258);
    /// A root was used exclusively (`inout`, mutation, or move) in the same
    /// full expression in which an accessor result borrows it. The accessor
    /// result's shared loan spans the enclosing full expression (ADR-0062),
    /// so the exclusive use violates the law of exclusivity.
    pub const ACCESSOR_LOAN_CONFLICT: Self = Self(259);
    /// An accessor declares a parameter mode other than by-value (`borrow`,
    /// `inout`, or `comptime` on a non-receiver parameter). Phase 1 accessor
    /// arguments are by-value guard inputs (ADR-0062); by-ref accessor
    /// parameters are deferred with the coroutine form (RUE-1012).
    pub const ACCESSOR_PARAM_MODE_UNSUPPORTED: Self = Self(260);
    /// An accessor call re-entered an accessor whose expansion is already in
    /// progress: `fn xr(borrow self) -> borrow i64 { yield self.xr(); }`, or
    /// the same cycle through several accessors. An accessor call compiles by
    /// inlining its body at the call site (ADR-0062), so an accessor-call
    /// cycle has no finite expansion.
    pub const ACCESSOR_RECURSION: Self = Self(261);

    // ========================================================================
    // Struct/enum errors (E0400-E0499)
    // ========================================================================
    pub const MISSING_FIELDS: Self = Self(400);
    pub const UNKNOWN_FIELD: Self = Self(401);
    pub const DUPLICATE_FIELD: Self = Self(402);
    pub const COPY_STRUCT_NON_COPY_FIELD: Self = Self(403);
    pub const RESERVED_TYPE_NAME: Self = Self(404);
    pub const DUPLICATE_TYPE_DEFINITION: Self = Self(405);
    pub const LINEAR_VALUE_NOT_CONSUMED: Self = Self(406);
    pub const LINEAR_STRUCT_COPY: Self = Self(407);
    // 408, 409 retired with the @handle directive (RUE-199).
    pub const DUPLICATE_METHOD: Self = Self(410);
    pub const UNDEFINED_METHOD: Self = Self(411);
    pub const UNDEFINED_ASSOC_FN: Self = Self(412);
    pub const METHOD_CALL_ON_NON_STRUCT: Self = Self(413);
    pub const METHOD_CALLED_AS_ASSOC_FN: Self = Self(414);
    pub const ASSOC_FN_CALLED_AS_METHOD: Self = Self(415);
    pub const DUPLICATE_DESTRUCTOR: Self = Self(416);
    pub const DESTRUCTOR_UNKNOWN_TYPE: Self = Self(417);
    pub const DUPLICATE_CONSTANT: Self = Self(418);
    pub const CONST_EXPR_NOT_SUPPORTED: Self = Self(434);
    pub const DUPLICATE_VARIANT: Self = Self(419);
    pub const UNKNOWN_VARIANT: Self = Self(420);
    pub const UNKNOWN_ENUM_TYPE: Self = Self(421);
    // 422 (FIELD_WRONG_ORDER) is retired: struct-literal fields may now be
    // given in any order, matched to declared fields by name (RUE-9). Do not
    // reuse the number.
    pub const FIELD_ACCESS_ON_NON_STRUCT: Self = Self(423);
    pub const INVALID_ASSIGNMENT_TARGET: Self = Self(424);
    pub const INOUT_NON_LVALUE: Self = Self(425);
    pub const INOUT_EXCLUSIVE_ACCESS: Self = Self(426);
    pub const BORROW_NON_LVALUE: Self = Self(427);
    pub const MUTATE_BORROWED_VALUE: Self = Self(428);
    pub const MOVE_OUT_OF_BORROW: Self = Self(429);
    pub const BORROW_INOUT_CONFLICT: Self = Self(430);
    pub const INOUT_KEYWORD_MISSING: Self = Self(431);
    pub const BORROW_KEYWORD_MISSING: Self = Self(432);
    pub const EMPTY_STRUCT: Self = Self(433);
    pub const RESERVED_FUNCTION_NAME: Self = Self(435);
    pub const DUPLICATE_FUNCTION_DEFINITION: Self = Self(436);
    pub const MOVE_OUT_OF_INOUT: Self = Self(437);
    // 438 (BY_REF_ARG_NOT_PLAIN_VARIABLE) is retired: by-ref arguments may
    // now be field/index projections (RUE-143). Do not reuse the number.
    // 439-441 are reserved by in-flight work; next free code is 444.
    pub const MOVE_SELF_OUT_OF_DESTRUCTOR: Self = Self(442);
    pub const LINEAR_VALUE_NOT_CONSUMED_ON_ALL_PATHS: Self = Self(443);
    // 444-455 are reserved by in-flight work; next free code is 458.
    pub const MOVE_FIELD_OUT_OF_DESTRUCTOR_TYPE: Self = Self(456);
    pub const COPY_STRUCT_WITH_DESTRUCTOR: Self = Self(457);
    // 458-459 are reserved by in-flight work.
    pub const PRIVATE_UNQUALIFIED_ACCESS: Self = Self(460);
    pub const CONST_INITIALIZER_CYCLE: Self = Self(461);
    // 462-473 are reserved by in-flight work.
    pub const LINEAR_FIELD_DROPPED_BY_DESTRUCTURE: Self = Self(474);
    pub const CONST_MISSING_TYPE_ANNOTATION: Self = Self(475);
    // 476-477 are reserved by in-flight work.
    pub const LINEAR_VALUE_DISCARDED: Self = Self(478);
    // 479 is reserved by in-flight work.
    pub const ASSIGN_TO_PARTIALLY_MOVED_ARRAY: Self = Self(480);
    /// An array length `[T; N]` where `N` is not a usable compile-time constant
    /// (a runtime variable, a negative/non-integer value, or an undefined
    /// name). Named lengths must resolve via the const evaluator (RUE-16).
    pub const INVALID_ARRAY_LENGTH: Self = Self(481);
    /// Source nests deeper than the compiler's fixed recursion limit
    /// (`MAX_NESTING_DEPTH`). A parser/AstGen guard reports this instead of
    /// overflowing the stack on pathologically nested input (RUE-42). It is a
    /// resource-limit diagnostic rather than a struct/enum error, but the
    /// reserved code lives in this block.
    pub const NESTING_LIMIT_EXCEEDED: Self = Self(482);
    /// A struct or enum (transitively) contains itself by value with no
    /// pointer indirection, so it has no finite size/layout (RUE-264).
    /// Analogous to Rust's E0072 and a sibling of [`Self::CONST_INITIALIZER_CYCLE`]
    /// (E0461) for the type-definition graph.
    pub const RECURSIVE_TYPE_INFINITE_SIZE: Self = Self(483);
    /// A pattern binds the same identifier more than once (e.g. an enum
    /// payload pattern `Rect(w, w)`). Every binding in a pattern must be a
    /// fresh name (spec 4.7:30); reusing one silently shadows the earlier
    /// binding and discards its value (RUE-269, analogous to Rust E0416).
    pub const DUPLICATE_PATTERN_BINDING: Self = Self(484);
    /// `@raw`/`@raw_mut` applied to a non-place operand (literal, arithmetic,
    /// call result). A raw pointer must address an addressable place (spec
    /// 9.1:12, ADR-0028); taking the "address" of a temporary value would
    /// reinterpret the value's bits as a pointer (RUE-274).
    pub const RAW_REQUIRES_PLACE: Self = Self(485);
    /// A second-class slice type (`borrow [T]` / `inout [T]`) or range slice
    /// (`x[a..b]`) was used with `--preview slices` enabled, but the
    /// fat-pointer runtime (ABI + codegen on both backends) is not yet
    /// implemented (ADR-0043 Phase 1, RUE-322). Emitted so the gated syntax
    /// fails cleanly instead of reaching an unfinished codegen path.
    ///
    /// Note: the second-class-escape diagnostics named in the RUE-322 brief
    /// (return / store-in-field / bind-past-scope) are allocated below
    /// (E0487-E0489); the brief's suggested E0435-E0437 numbers were already
    /// taken (`RESERVED_FUNCTION_NAME` / `DUPLICATE_FUNCTION_DEFINITION` /
    /// `MOVE_OUT_OF_INOUT`).
    pub const SLICE_NOT_YET_IMPLEMENTED: Self = Self(486);
    /// A slice type `[T]` was written in return position (`fn f(...) -> [T]`).
    /// A slice is a *second-class* fat-pointer view (ADR-0037, ADR-0043,
    /// RUE-322): it is valid only in argument position and may not be
    /// returned, since the view would outlive the borrow it aliases.
    pub const SLICE_RETURN_NOT_ALLOWED: Self = Self(487);
    /// A slice type `[T]` was written as a struct field type. A slice is
    /// second-class (ADR-0037, ADR-0043, RUE-322) and cannot be stored in an
    /// aggregate — storing it would let the view escape its borrow's scope.
    pub const SLICE_IN_AGGREGATE_FIELD: Self = Self(488);
    /// A slice type `[T]` was written in a binding position other than a
    /// parameter — a `let` local or a `const`. A slice is second-class
    /// (ADR-0037, ADR-0043, RUE-322): it may only name a function parameter,
    /// so it cannot be bound past its argument scope.
    pub const SLICE_ESCAPES_SCOPE: Self = Self(489);
    /// `@field_ptr(...)` was applied to something other than a field-access
    /// expression `s.field` (RUE-301). `@field_ptr` is *compiler-mediated
    /// field access*: it forms a raw pointer to the field the compiler placed,
    /// so its operand MUST name a struct field. Use `@raw`/`@raw_mut` for the
    /// address of a whole variable or array element.
    pub const FIELD_PTR_REQUIRES_FIELD: Self = Self(490);
    /// A function or method parameter list names the same parameter twice
    /// (e.g. `fn f(x: i32, x: i32)`). Every parameter in a single list must
    /// have a distinct name (spec 6.1); a repeated name silently shadows the
    /// earlier binding on a first-wins basis, so the declaration is rejected
    /// (RUE-349, a sibling of [`Self::DUPLICATE_PATTERN_BINDING`] (E0484) and
    /// analogous to Rust's E0415).
    pub const DUPLICATE_PARAMETER: Self = Self(491);
    /// A string literal assigned to a fixed-capacity string `Str(N)` does not
    /// fit: its UTF-8 byte length exceeds the capacity `N` (ADR-0043 Phase 5,
    /// RUE-326). `Str(N)` is the fixed string rung (`[u8; N]` + UTF-8) with no
    /// heap, so an over-long literal cannot be stored — the fit is checked at
    /// compile time.
    pub const STR_FIXED_CAPACITY_EXCEEDED: Self = Self(492);
    /// Assignment to an initialized place that holds a live linear value
    /// (RUE-387). Overwriting the place would implicitly drop (silently
    /// consume) the old linear value, which linearity forbids — a theorem-5
    /// soundness hole (spec 3.9:18 overwrite-drop is carved out for linear
    /// types). The value must be moved/consumed explicitly first; the sole
    /// exception is reinitializing a place that was provably moved out on
    /// every path (the spec 3.8:55/56 reinit idiom), which holds nothing to
    /// destroy.
    pub const LINEAR_VALUE_OVERWRITTEN: Self = Self(493);
    /// Assignment to an `inout` parameter (or a place rooted at one) whose
    /// type carries a linear value (RUE-387). The parameter names the
    /// caller's storage, which holds a live linear value; reassigning it
    /// would implicitly drop that value in the caller. An `inout` place can
    /// never be proven moved-out (moving out of a by-ref binding is itself
    /// rejected), so a linear `inout` assignment is always ill-formed.
    pub const LINEAR_VALUE_OVERWRITTEN_THROUGH_INOUT: Self = Self(494);
    /// A string *buffer* — `StrBuf` (growable) or `Str(N)` (fixed) — was used
    /// where a *first-class* `str` value is required: a bare `str` parameter,
    /// a `str` binding, a `str` return, or a `str` struct field (ADR-0043
    /// two-types model, RUE-386). A first-class `str` is static-backed and may
    /// escape; a buffer's bytes live in caller-owned local/heap storage, so
    /// letting it become a first-class `str` produces a dangling view once the
    /// buffer is dropped (the verified RUE-386 segfault). A buffer coerces only
    /// to a second-class `borrow str` / `inout str` view.
    pub const BUFFER_NOT_FIRST_CLASS_STR: Self = Self(495);
    /// A first-class / static-backed `str` value was supplied as the operand
    /// of an `inout str` parameter (ADR-0043 two-types model, RUE-386). An
    /// exclusive `str` view requires *local* provenance — a `StrBuf`/`Str(N)`
    /// buffer the caller owns — because a static `str` lives in read-only
    /// `.rodata` (exclusive mutation would fault) and, being `Copy`, can have
    /// two roots over one static buffer that per-root exclusivity cannot see.
    pub const INOUT_STR_REQUIRES_LOCAL_BUFFER: Self = Self(496);
    /// A borrowed `str` *view* — the binding of a `borrow str` / `inout str`
    /// parameter — was used where a first-class `str` value is required
    /// (returned, stored in a struct field, bound to a `str` local, or passed
    /// to a bare `str` parameter) (ADR-0043 two-types model, RUE-386). A view
    /// is second-class: it may only be read (`.len()`, byte indexing) or
    /// re-borrowed, never escape the call as a first-class value.
    pub const STR_VIEW_NOT_FIRST_CLASS: Self = Self(497);
    // E0498 (CONTAINER_ELEMENT_HAS_DESTRUCTOR) was retired by RUE-646: owning
    // growable containers now run each live element's drop glue before freeing
    // their buffer (Rust's `Vec<T>` discipline), so a destructor-bearing element
    // type is legal. The code number is left retired (a gap is fine) rather than
    // reused. Linear elements are still rejected — see E0499 below.
    /// An owning growable container (e.g. `ArrayBuf(T)`) was instantiated with a
    /// `linear` element type, via the `@require_droppable(T)` gate. Until
    /// container/element multiplicity propagation is designed (RUE-388), such
    /// containers cannot yet track element linearity, so a linear element would
    /// be leaked (never consumed); the instantiation is rejected instead.
    pub const CONTAINER_ELEMENT_IS_LINEAR: Self = Self(499);

    // ========================================================================
    // Control flow errors (E0500-E0599)
    // ========================================================================
    pub const BREAK_OUTSIDE_LOOP: Self = Self(500);
    pub const CONTINUE_OUTSIDE_LOOP: Self = Self(501);
    pub const BREAK_WITH_VALUE: Self = Self(502);
    /// The `?` operator was used in a function whose return type is not an
    /// `Option` (RUE-6, ADR-0038): `?` early-returns `None`, so the enclosing
    /// function must return an `Option`.
    pub const QUESTION_OUTSIDE_OPTION_FN: Self = Self(503);
    pub const QUESTION_OUTSIDE_RESULT_FN: Self = Self(505);
    pub const QUESTION_ERR_TYPE_MISMATCH: Self = Self(506);
    /// The `?` operator was applied to a value that is not an `Option`
    /// (RUE-6, ADR-0038).
    pub const QUESTION_ON_NON_OPTION: Self = Self(504);

    // ========================================================================
    // Match errors (E0600-E0699)
    // ========================================================================
    pub const NON_EXHAUSTIVE_MATCH: Self = Self(600);
    pub const EMPTY_MATCH: Self = Self(601);
    pub const INVALID_MATCH_TYPE: Self = Self(602);

    // ========================================================================
    // Intrinsic errors (E0700-E0799)
    // ========================================================================
    pub const UNKNOWN_INTRINSIC: Self = Self(700);
    pub const INTRINSIC_WRONG_ARG_COUNT: Self = Self(701);
    pub const INTRINSIC_TYPE_MISMATCH: Self = Self(702);
    pub const IMPORT_REQUIRES_STRING_LITERAL: Self = Self(703);
    pub const MODULE_NOT_FOUND: Self = Self(704);
    pub const STD_LIB_NOT_FOUND: Self = Self(705);
    pub const PRIVATE_MEMBER_ACCESS: Self = Self(706);
    pub const UNKNOWN_MODULE_MEMBER: Self = Self(707);
    pub const AMBIGUOUS_MODULE: Self = Self(708);
    pub const CANNOT_INFER_CAST_TARGET: Self = Self(709);
    pub const CANNOT_INFER_POINTEE_TYPE: Self = Self(710);
    pub const CONTAINER_ELEMENT_NOT_TRIVIALLY_DROPPABLE: Self = Self(711);
    /// A comptime-constant `align` argument to a byte-allocation intrinsic
    /// (`@alloc`/`@alloc_zeroed`/`@free`/`@realloc`/`@resize`, ADR-0059) was
    /// zero or not a power of two. Alignment must be a power of two.
    pub const INTRINSIC_ALIGN_NOT_POWER_OF_TWO: Self = Self(712);

    // ========================================================================
    // Literal/operator errors (E0800-E0899)
    // ========================================================================
    pub const LITERAL_OUT_OF_RANGE: Self = Self(800);
    pub const CANNOT_NEGATE: Self = Self(801);
    pub const CHAINED_COMPARISON: Self = Self(802);

    // ========================================================================
    // Array errors (E0900-E0999)
    // ========================================================================
    pub const INDEX_ON_NON_ARRAY: Self = Self(900);
    pub const ARRAY_LENGTH_MISMATCH: Self = Self(901);
    pub const INDEX_OUT_OF_BOUNDS: Self = Self(902);
    pub const TYPE_ANNOTATION_REQUIRED: Self = Self(903);
    pub const MOVE_OUT_OF_INDEX: Self = Self(904);
    pub const ARRAY_REPEAT_NON_COPY: Self = Self(905);
    pub const TYPE_TOO_LARGE: Self = Self(906);
    pub const FUNCTION_FRAME_TOO_LARGE: Self = Self(907);

    // ------------------------------------------------------------------
    // Bit-reinterpretation errors (E0950-E0959)
    // ------------------------------------------------------------------
    /// `@bitCast` was written between integer types of different widths
    /// (RUE-952, spec 4.13:120). A bit reinterpretation renames the bits it is
    /// given; it neither invents nor discards any, so the source and target
    /// **must** share a width. The value-changing conversion is `@intCast`.
    pub const BIT_CAST_WIDTH_MISMATCH: Self = Self(950);

    // ========================================================================
    // Linker/target errors (E1000-E1099)
    // ========================================================================
    pub const LINK_ERROR: Self = Self(1000);
    pub const UNSUPPORTED_TARGET: Self = Self(1001);

    // ========================================================================
    // Preview feature errors (E1100-E1199)
    // ========================================================================
    pub const PREVIEW_FEATURE_REQUIRED: Self = Self(1100);
    pub const EXTERN_SIGNATURE_TYPE_UNSUPPORTED: Self = Self(1101);
    pub const EXTERN_AGGREGATE_NOT_REPR_C: Self = Self(1102);
    pub const EXTERN_ARRAY_BY_VALUE: Self = Self(1103);
    pub const REPR_C_STRUCT_INELIGIBLE: Self = Self(1104);
    pub const EXTERN_VARIADIC_UNSUPPORTED: Self = Self(1105);
    pub const EXPORT_SIGNATURE_UNSUPPORTED: Self = Self(1106);
    /// The same C symbol is declared by two `extern "C"` foreign declarations
    /// whose Rue signatures disagree (RUE-1218, spec 9.3:5). A foreign
    /// declaration names an external C symbol rather than a Rue callable, so
    /// every module that declares it is describing *one* function; disagreeing
    /// descriptions cannot both be right, and the program links against a
    /// single definition. Sits in the FFI band beside the other `extern "C"`
    /// signature rules (E1101-E1103) because it is a foreign-declaration rule,
    /// not a general duplicate-definition rule — an ordinary function's
    /// identity is module-local (RUE-1125), so only foreign declarations can
    /// collide across modules at all.
    pub const FOREIGN_SIGNATURE_CONFLICT: Self = Self(1107);
    /// An `extern "C"` foreign declaration names `main`, the program entry
    /// point (RUE-1220, spec 9.3:6). A foreign declaration names the C symbol
    /// it declares (RUE-1125), so this one takes the bare `main` symbol the
    /// runtime start glue calls (spec 6.1:38) — in the root module it collides
    /// with the entry point's own definition, and in any other module a call
    /// through it recurses into the program's own `main`. Sits in the FFI band
    /// beside the other `extern "C"` rules, and is the import-side mirror of
    /// E1106's rejection of a `pub extern "C" fn` export named `main`.
    pub const FOREIGN_ENTRY_POINT_DECLARATION: Self = Self(1108);
    /// A float literal was used with `--preview floats` enabled, but the
    /// typing phases of ADR-0065 (Phase 4 onward) are not implemented yet.
    /// Sits in the preview band beside E1100 because it is the second half of
    /// the same gate: E1100 fires without the flag, E1109 with it. (RUE-1069)
    pub const FLOAT_NOT_YET_IMPLEMENTED: Self = Self(1109);

    // ========================================================================
    // Comptime errors (E1200-E1299)
    // ========================================================================
    pub const COMPTIME_EVALUATION_FAILED: Self = Self(1200);
    pub const COMPTIME_ARG_NOT_CONST: Self = Self(1201);

    // ========================================================================
    // Unchecked-code errors (E1300-E1399)
    // ========================================================================
    pub const UNCHECKED_OP_REQUIRES_CHECKED: Self = Self(1300);

    // ========================================================================
    // Compiler-input errors (E1400-E1499)
    // ========================================================================
    /// Invalid metadata supplied at a compiler API boundary.
    pub const INVALID_COMPILER_INPUT: Self = Self(1400);
    pub const COMPILER_RESOURCE_LIMIT: Self = Self(1401);
    pub const COMPILER_RESOURCE_EXHAUSTION: Self = Self(1402);
    pub const OUTPUT_PUBLICATION: Self = Self(1403);
    pub const UNSATISFIED_TRUSTED_TOOLCHAIN_INPUT: Self = Self(1404);

    // ========================================================================
    // Internal compiler errors (E9000-E9999)
    // ========================================================================
    pub const INTERNAL_ERROR: Self = Self(9000);
    pub const INTERNAL_CODEGEN_ERROR: Self = Self(9001);
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04}", self.0)
    }
}

// ============================================================================
// Boxed Error Payloads
// ============================================================================
//
// # Boxing Policy
//
// Large error variants are boxed to reduce the size of ErrorKind.
// This keeps Result<T, CompileError> smaller on the stack.
// Errors are cold paths, so the extra indirection is acceptable.
//
// ## When to Box
//
// Box error payloads when the variant data is **≥ 72 bytes** (3 or more Strings).
//
// Basic sizes on 64-bit systems:
// - String: 24 bytes
// - Vec<T>: 24 bytes
// - Box<T>: 8 bytes (pointer)
// - Cow<'static, str>: 24 bytes
//
// Examples:
// - 1 String: 24 bytes → inline
// - 2 Strings: 48 bytes → inline
// - 3 Strings: 72 bytes → **box**
// - String + Vec<String>: 48 bytes → inline (unless Vec typically large)
//
// ## Pattern
//
// Use a dedicated struct for boxed payloads:
//
// ```rust
// #[derive(Debug, Clone, PartialEq, Eq)]
// pub struct LargeErrorPayload {
//     pub field1: String,
//     pub field2: String,
//     pub field3: String,
// }
//
// #[error("message")]
// LargeError(Box<LargeErrorPayload>),
// ```
//
// ## Current Status
//
// As of 2026-01-11:
// - ErrorKind size: 56 bytes
// - Boxed variants: 3 (MissingFields, CopyStructNonCopyField,
//   IntrinsicTypeMismatch)
// - All boxed variants contain 3+ Strings or String + Vec
// - Policy is consistently applied

/// Payload for `ErrorKind::MissingFields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingFieldsError {
    pub struct_name: String,
    pub missing_fields: Vec<String>,
}

/// Payload for `ErrorKind::CopyStructNonCopyField`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyStructNonCopyFieldError {
    pub struct_name: String,
    pub field_name: String,
    pub field_type: String,
}

/// Payload for `ErrorKind::ReprCStructIneligible`.
///
/// A `@repr(c)` struct failed the reject-don't-guess eligibility check
/// (ADR-0064 Amendment 1). The structured `field_path` mirrors the failing
/// predicate's field path (the RUE-504 machine-readable exposure direction);
/// `reason` is the rendered human explanation naming the reject-list entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReprCIneligibleError {
    /// The `@repr(c)` struct that is not FFI-eligible.
    pub struct_name: String,
    /// Dotted field path to the offending field, empty when the struct body
    /// itself is the problem (e.g. an empty struct).
    pub field_path: String,
    /// The offending type as rendered for the user.
    pub failing_type: String,
    /// Human phrase naming the reject-list reason.
    pub reason: String,
}

/// Payload for `ErrorKind::ForeignSignatureConflict`.
///
/// Two `extern "C"` foreign declarations name the same C symbol with
/// disagreeing Rue signatures (RUE-1218). Both rendered signatures are carried
/// so the diagnostic states the disagreement rather than only pointing at the
/// two declaration sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignSignatureConflictError {
    /// The C symbol both declarations name.
    pub symbol: String,
    /// The later declaration's signature, as rendered for the user.
    pub declared: String,
    /// The earlier declaration's signature, as rendered for the user.
    pub previously_declared: String,
}

/// Payload for `ErrorKind::LinearFieldDroppedByDestructure`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearFieldDroppedByDestructureError {
    pub struct_name: String,
    pub accessed: String,
    pub dropped: String,
}

/// Payload for `ErrorKind::IntrinsicTypeMismatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrinsicTypeMismatchError {
    pub name: String,
    pub expected: String,
    pub found: String,
}

// ============================================================================
// Preview Features
// ============================================================================

/// A preview feature that can be enabled with `--preview`.
///
/// Preview features are in-progress language additions that:
/// - May change or be removed before stabilization
/// - Require explicit opt-in via `--preview <feature>`
/// - Allow incremental implementation to be merged to main
///
/// See ADR-0005 for the full design.
///
/// When all preview features are stabilized, this enum may be empty.
/// New preview features are added here as development begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PreviewFeature {
    /// Testing infrastructure feature - permanently unstable.
    /// Used to verify the preview feature gating mechanism works.
    TestInfra,
    /// Second-class slice types `borrow [T]` / `inout [T]` and range slicing
    /// `x[a..b]` (ADR-0043, RUE-322). The keystone view type of the
    /// collection/string trio. Gated until the fat-pointer ABI lands on both
    /// backends.
    Slices,
    /// C FFI: `extern "C"` foreign-function declarations and static-archive
    /// linking (ADR-0064, RUE-1055). Gated until every phase of the guaranteed
    /// target-C boundary is proven on both backends.
    CFfi,
    /// Place-returning borrow accessors (ADR-0062, RUE-662): methods with a
    /// `-> borrow T` result whose `yield`-body hands out a second-class borrow
    /// of a projection of the receiver. Gated until mutable accessors and std
    /// adoption complete the rollout (RUE-1015).
    BorrowAccessors,
    /// Floating point: `f32`/`f64`, IEEE-754 arithmetic, and `comptime_float`
    /// literals (ADR-0065, RUE-714). Gated until every phase of the M9 rollout
    /// — types and inference, both backends, the dtoa runtime — is complete
    /// (ADR-0065 Phase 10). The lexer and parser accept a float literal only
    /// with this flag; the phases that would give it a type do not exist yet,
    /// so an enabled float literal still stops at
    /// [`ErrorKind::FloatNotYetImplemented`].
    Floats,
}

/// Error returned when parsing a preview feature name fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsePreviewFeatureError(String);

impl fmt::Display for ParsePreviewFeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown preview feature '{}'", self.0)
    }
}

impl std::error::Error for ParsePreviewFeatureError {}

impl PreviewFeature {
    /// Get the CLI name for this feature (used with `--preview`).
    #[allow(unreachable_code)]
    pub fn name(&self) -> &'static str {
        match *self {
            PreviewFeature::TestInfra => "test_infra",
            PreviewFeature::Slices => "slices",
            PreviewFeature::CFfi => "c_ffi",
            PreviewFeature::BorrowAccessors => "borrow_accessors",
            PreviewFeature::Floats => "floats",
        }
    }

    /// Get the ADR number documenting this feature.
    #[allow(unreachable_code)]
    pub fn adr(&self) -> &'static str {
        match *self {
            PreviewFeature::TestInfra => "ADR-0005",
            PreviewFeature::Slices => "ADR-0043",
            PreviewFeature::CFfi => "ADR-0064",
            PreviewFeature::BorrowAccessors => "ADR-0062",
            PreviewFeature::Floats => "ADR-0065",
        }
    }

    /// Get all available preview features.
    pub fn all() -> &'static [PreviewFeature] {
        &[
            PreviewFeature::TestInfra,
            PreviewFeature::Slices,
            PreviewFeature::CFfi,
            PreviewFeature::BorrowAccessors,
            PreviewFeature::Floats,
        ]
    }

    /// Get a comma-separated list of all feature names (for help text).
    pub fn all_names() -> String {
        if Self::all().is_empty() {
            "(none)".to_string()
        } else {
            Self::all()
                .iter()
                .map(|f| f.name())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

impl std::str::FromStr for PreviewFeature {
    type Err = ParsePreviewFeatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "test_infra" => Ok(PreviewFeature::TestInfra),
            "slices" => Ok(PreviewFeature::Slices),
            "c_ffi" => Ok(PreviewFeature::CFfi),
            "borrow_accessors" => Ok(PreviewFeature::BorrowAccessors),
            "floats" => Ok(PreviewFeature::Floats),
            _ => Err(ParsePreviewFeatureError(s.to_string())),
        }
    }
}

impl fmt::Display for PreviewFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A set of enabled preview features.
pub type PreviewFeatures = HashSet<PreviewFeature>;

// ============================================================================
// Diagnostic Types
// ============================================================================

/// A secondary label pointing to related code.
///
/// Labels appear as additional annotations in the source snippet,
/// helping users understand the relationship between different parts of code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// The message explaining this location's relevance.
    pub message: String,
    /// The source location to highlight.
    pub span: Span,
}

impl Label {
    /// Create a new label with a message and span.
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

/// An informational note providing context.
///
/// Notes appear as footer messages and explain why something happened
/// or provide additional context about the diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note(pub String);

impl Note {
    /// Create a new note.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An actionable help suggestion.
///
/// Helps appear as footer messages and suggest specific actions
/// the user can take to resolve the issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help(pub String);

impl Help {
    /// Create a new help suggestion.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Help {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How confident we are that a suggested fix is correct.
///
/// This follows rustc's conventions for suggestion applicability levels.
/// IDEs and tools can use this to decide whether to auto-apply suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Applicability {
    /// The suggestion is definitely correct and can be safely auto-applied.
    ///
    /// Use this when the fix is guaranteed to compile and preserve semantics.
    MachineApplicable,

    /// The suggestion might be correct but should be reviewed by a human.
    ///
    /// Use this when the fix will likely work but may change behavior in
    /// edge cases, or when there are multiple equally valid options.
    MaybeIncorrect,

    /// The suggestion contains placeholders that the user must fill in.
    ///
    /// Use this when the fix shows the general shape but needs specific
    /// values like variable names or types.
    HasPlaceholders,

    /// The suggestion is just a hint and may not even compile.
    ///
    /// Use this for illustrative suggestions that show concepts rather
    /// than working code.
    Unspecified,
}

impl Default for Applicability {
    fn default() -> Self {
        Self::Unspecified
    }
}

impl std::fmt::Display for Applicability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Applicability::MachineApplicable => write!(f, "MachineApplicable"),
            Applicability::MaybeIncorrect => write!(f, "MaybeIncorrect"),
            Applicability::HasPlaceholders => write!(f, "HasPlaceholders"),
            Applicability::Unspecified => write!(f, "Unspecified"),
        }
    }
}

/// A suggested code fix that can be applied to resolve a diagnostic.
///
/// Suggestions provide machine-readable fix information that IDEs and
/// tools can use to offer quick-fix actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Human-readable description of what the suggestion does.
    pub message: String,
    /// The span of code to replace.
    pub span: Span,
    /// The replacement text.
    pub replacement: String,
    /// How confident we are that this fix is correct.
    pub applicability: Applicability,
}

impl Suggestion {
    /// Create a new suggestion with unspecified applicability.
    pub fn new(message: impl Into<String>, span: Span, replacement: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::Unspecified,
        }
    }

    /// Create a suggestion that is safe to auto-apply.
    pub fn machine_applicable(
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::MachineApplicable,
        }
    }

    /// Create a suggestion that may need human review.
    pub fn maybe_incorrect(
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::MaybeIncorrect,
        }
    }

    /// Create a suggestion with placeholders.
    pub fn with_placeholders(
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            message: message.into(),
            span,
            replacement: replacement.into(),
            applicability: Applicability::HasPlaceholders,
        }
    }

    /// Set the applicability of this suggestion.
    pub fn with_applicability(mut self, applicability: Applicability) -> Self {
        self.applicability = applicability;
        self
    }
}

/// Rich diagnostic information for errors and warnings.
///
/// This struct collects all supplementary information that can be
/// attached to a diagnostic message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostic {
    /// Secondary labels pointing to related code locations.
    pub labels: Vec<Label>,
    /// Informational notes providing context.
    pub notes: Vec<Note>,
    /// Actionable help suggestions.
    pub helps: Vec<Help>,
    /// Code suggestions that can be applied to fix the issue.
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    /// Create an empty diagnostic.
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if this diagnostic has any content.
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
            && self.notes.is_empty()
            && self.helps.is_empty()
            && self.suggestions.is_empty()
    }
}

// ============================================================================
// Generic Diagnostic Wrapper
// ============================================================================

/// A compilation diagnostic (error or warning) with optional source location.
///
/// This is a generic wrapper that holds a diagnostic kind along with optional
/// source location and rich diagnostic information (labels, notes, helps).
///
/// Use the type aliases [`CompileError`] and [`CompileWarning`] for the
/// specific error and warning types.
///
/// Diagnostics can include rich information using the builder methods:
/// ```ignore
/// CompileError::new(ErrorKind::TypeMismatch { ... }, span)
///     .with_label("expected because of this", other_span)
///     .with_note("types must match exactly")
///     .with_help("consider adding a type conversion")
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "compiler diagnostics should not be ignored"]
pub struct DiagnosticWrapper<K> {
    /// The specific kind of diagnostic.
    pub kind: K,
    span: Option<Span>,
    diagnostic: Diagnostic,
}

impl<K> DiagnosticWrapper<K> {
    /// Create a new diagnostic with the given kind and span.
    #[inline]
    pub fn new(kind: K, span: Span) -> Self {
        Self {
            kind,
            span: Some(span),
            diagnostic: Diagnostic::new(),
        }
    }

    /// Create a diagnostic without a source location.
    ///
    /// Use this for diagnostics that don't correspond to a specific source
    /// location, such as "no main function found" or linker errors.
    #[inline]
    pub fn without_span(kind: K) -> Self {
        Self {
            kind,
            span: None,
            diagnostic: Diagnostic::new(),
        }
    }

    /// Returns true if this diagnostic has source location information.
    #[inline]
    pub fn has_span(&self) -> bool {
        self.span.is_some()
    }

    /// Get the span, if present.
    #[inline]
    pub fn span(&self) -> Option<Span> {
        self.span
    }

    /// Get the diagnostic information.
    #[inline]
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }

    /// Rebind every source location carried by this diagnostic while
    /// preserving its kind and explanatory payload.
    pub fn map_spans(mut self, mut map: impl FnMut(Span) -> Span) -> Self {
        self.span = self.span.map(&mut map);
        for label in &mut self.diagnostic.labels {
            label.span = map(label.span);
        }
        for suggestion in &mut self.diagnostic.suggestions {
            suggestion.span = map(suggestion.span);
        }
        self
    }

    /// Add a secondary label pointing to related code.
    ///
    /// Labels appear as additional annotations in the source snippet.
    #[inline]
    pub fn with_label(mut self, message: impl Into<String>, span: Span) -> Self {
        self.diagnostic.labels.push(Label::new(message, span));
        self
    }

    /// Add an informational note.
    ///
    /// Notes appear as footer messages providing context.
    #[inline]
    pub fn with_note(mut self, message: impl Into<String>) -> Self {
        self.diagnostic.notes.push(Note::new(message));
        self
    }

    /// Add a help suggestion.
    ///
    /// Helps appear as footer messages with actionable advice.
    #[inline]
    pub fn with_help(mut self, message: impl Into<String>) -> Self {
        self.diagnostic.helps.push(Help::new(message));
        self
    }

    /// Add a code suggestion that can be applied to fix the issue.
    ///
    /// Suggestions provide machine-readable fix information for IDEs and tools.
    #[inline]
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.diagnostic.suggestions.push(suggestion);
        self
    }
}

impl<K: fmt::Display> fmt::Display for DiagnosticWrapper<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl<K: fmt::Display + fmt::Debug> std::error::Error for DiagnosticWrapper<K> {}

// ============================================================================
// Compile Errors
// ============================================================================

/// A compilation error with optional source location information.
///
/// Some errors (like `NoMainFunction` or `LinkError`) don't have a meaningful
/// source location. Use `has_span()` to check before rendering location info.
///
/// Errors can include rich diagnostic information using the builder methods:
/// ```ignore
/// CompileError::new(ErrorKind::TypeMismatch { ... }, span)
///     .with_label("expected because of this", other_span)
///     .with_note("types must match exactly")
///     .with_help("consider adding a type conversion")
/// ```
pub type CompileError = DiagnosticWrapper<ErrorKind>;

// Helper functions for complex error formatting in thiserror attributes

fn format_argument_count(expected: usize, found: usize) -> String {
    if expected == 1 {
        format!("expected {} argument, found {}", expected, found)
    } else {
        format!("expected {} arguments, found {}", expected, found)
    }
}

fn format_missing_fields(err: &MissingFieldsError) -> String {
    if err.missing_fields.len() == 1 {
        format!(
            "missing field '{}' in struct '{}'",
            err.missing_fields[0], err.struct_name
        )
    } else {
        let fields = err
            .missing_fields
            .iter()
            .map(|f| format!("'{}'", f))
            .collect::<Vec<_>>()
            .join(", ");
        format!("missing fields {} in struct '{}'", fields, err.struct_name)
    }
}

fn format_intrinsic_arg_count(name: &str, expected: usize, found: usize) -> String {
    if expected == 1 {
        format!(
            "intrinsic '@{}' expects {} argument, found {}",
            name, expected, found
        )
    } else {
        format!(
            "intrinsic '@{}' expects {} arguments, found {}",
            name, expected, found
        )
    }
}

fn format_array_length_mismatch(expected: u64, found: u64) -> String {
    if expected == 1 {
        format!(
            "expected array of {} element, found {} elements",
            expected, found
        )
    } else {
        format!(
            "expected array of {} elements, found {} elements",
            expected, found
        )
    }
}

/// Payload for [`ErrorKind::AmbiguousModule`], boxed to keep `ErrorKind`
/// within its 64-byte size budget (three inline `String`s exceed it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousModuleData {
    pub path: String,
    pub file_module: String,
    pub dir_module: String,
}

/// Payload for [`ErrorKind::PrivateUnqualifiedAccess`], boxed to keep
/// `ErrorKind` within its 64-byte size budget (three inline `String`s
/// exceed it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateUnqualifiedAccessData {
    /// The kind of item ("function", "struct", "enum", "constant").
    pub item_kind: String,
    /// The item's name as written at the reference site.
    pub name: String,
    /// The path of the file that defines the private item.
    pub defining_file: String,
}

/// The kind of compilation error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ErrorKind {
    // Lexer errors
    // escape_debug keeps printable characters as-is but renders control and
    // other invisible characters (NUL, VT, BOM, ...) as visible escapes like
    // \u{b}, so a hostile byte can't corrupt the one-line message.
    #[error("unexpected character: {}", .0.escape_debug())]
    UnexpectedCharacter(char),
    #[error("invalid integer literal")]
    InvalidInteger,
    #[error("invalid escape sequence: \\{}", .0.escape_debug())]
    InvalidStringEscape(char),
    #[error("unterminated string literal")]
    UnterminatedString,
    /// An uppercase integer-literal base prefix (`0X`/`0B`/`0O`). Base
    /// prefixes are lowercase (spec 2.1); a targeted error is friendlier
    /// than splitting into `0` + identifier and dying with a generic parse
    /// error. (RUE-177)
    #[error("invalid base prefix `0{0}`: base prefixes are lowercase (`0x`, `0o`, `0b`)")]
    UppercaseBasePrefix(char),
    /// A based integer literal with no digits after the prefix (`0x`,
    /// `0b_`). (RUE-177)
    #[error("missing digits after base prefix in {base} integer literal")]
    EmptyBasedLiteral { base: &'static str },
    /// A digit that is not valid for the literal's base (`0b2`, `0o9`,
    /// `0xG`). (RUE-177)
    #[error("invalid digit `{digit}` in {base} integer literal")]
    InvalidDigitForBase { digit: char, base: &'static str },
    /// A malformed byte literal (`b'ab'`, `b''`, `b'\q'`, `b'é'`, an
    /// unterminated `b'a`). Carries a specific, already-rendered reason built
    /// by the lexer. Byte literals (`b'a'`) are readable `u8` spellings of a
    /// single ASCII byte (RUE-1042).
    #[error("{0}")]
    MalformedByteLiteral(String),
    /// A malformed floating-point literal: a leading dot (`.5`) or a trailing
    /// dot (`5.`). ADR-0065 §3 rejects both spellings for readability — write
    /// `0.5` and `5.0`. Carries a specific, already-rendered reason.
    ///
    /// The leading-dot form is a lexical decision (`.` immediately followed by
    /// a digit can never start any other Rue lexeme), so the lexer reports it.
    /// The trailing-dot form is not decidable from the lexeme alone: `42.` is
    /// the prefix of the legal method call `42.to_string()`, so the *parser*
    /// reports it once the token after the `.` proves no member name follows.
    /// Both share this kind — and E0011 — because both diagnose the same
    /// numeric-literal spelling rule.
    #[error("{0}")]
    MalformedFloatLiteral(String),
    /// Summary emitted after lexing reaches its per-file diagnostic budget.
    /// `limit` is the number of detailed diagnostics retained.
    #[error("additional lexer diagnostics omitted after the first {limit} errors")]
    LexerDiagnosticsOmitted { limit: usize },

    // Parser errors
    #[error("expected {expected}, found {found}")]
    UnexpectedToken {
        expected: Cow<'static, str>,
        found: Cow<'static, str>,
    },
    /// A custom parse error with a specific message.
    ///
    /// Used for parser-generated errors that don't fit the "expected X, found Y" pattern.
    #[error("{0}")]
    ParseError(String),
    /// Summary emitted after parser recovery reaches its per-file diagnostic
    /// budget. `limit` is the number of detailed diagnostics retained.
    #[error("additional parser diagnostics omitted after the first {limit} errors")]
    ParserDiagnosticsOmitted { limit: usize },
    /// Source is nested more deeply than the compiler supports. Reported by
    /// the parser's nesting pre-scan and by the AstGen depth guard so that
    /// pathologically nested input yields a clean diagnostic instead of a
    /// stack overflow (RUE-42). `limit` is the maximum supported nesting
    /// depth (`MAX_NESTING_DEPTH`).
    #[error("expression nests too deeply; exceeds the maximum nesting depth of {limit}")]
    NestingLimitExceeded { limit: usize },

    // Semantic errors
    #[error("no main function found")]
    NoMainFunction,
    /// The `main` declaration does not match the runtime entry ABI (RUE-778).
    #[error("invalid main function signature: {reason}")]
    InvalidMainSignature { reason: &'static str },
    #[error("undefined variable '{0}'")]
    UndefinedVariable(String),
    #[error("undefined function '{0}'")]
    UndefinedFunction(String),
    #[error("cannot assign to immutable variable '{0}'")]
    AssignToImmutable(String),
    #[error("unknown type '{0}'")]
    UnknownType(String),
    /// An array length `[T; N]` where `N` is not a usable compile-time
    /// constant: a runtime variable, a non-integer/negative value, or an
    /// undefined name (RUE-16). `reason` explains the specific problem.
    #[error("invalid array length: {reason}")]
    InvalidArrayLength { reason: String },
    /// Use of a value after it has been moved.
    #[error("use of moved value '{0}'")]
    UseAfterMove(String),
    #[error("type mismatch: expected {expected}, found {found}")]
    TypeMismatch { expected: String, found: String },
    #[error("{}", format_argument_count(*.expected, *.found))]
    WrongArgumentCount { expected: usize, found: usize },
    /// A pattern binds the same identifier more than once (spec 4.7:30):
    /// e.g. an enum payload pattern `Rect(w, w)`. Reusing a name silently
    /// shadows the earlier binding and discards its value (RUE-269).
    #[error("identifier '{name}' is bound more than once in the same pattern")]
    DuplicatePatternBinding { name: String },

    // Struct errors
    #[error("{}", format_missing_fields(.0))]
    MissingFields(Box<MissingFieldsError>),
    #[error("unknown field '{field_name}' in struct '{struct_name}'")]
    UnknownField {
        struct_name: String,
        field_name: String,
    },
    #[error("duplicate field '{field_name}' in struct '{struct_name}'")]
    DuplicateField {
        struct_name: String,
        field_name: String,
    },
    /// Anonymous struct with no fields is not allowed
    #[error("empty struct is not allowed")]
    EmptyStruct,
    /// @copy struct contains a field with non-Copy type
    #[error("@copy struct '{struct_name}' has field '{field_name}' with non-Copy type '{field_type}'", struct_name = .0.struct_name, field_name = .0.field_name, field_type = .0.field_type)]
    CopyStructNonCopyField(Box<CopyStructNonCopyFieldError>),
    /// User-defined type collides with a built-in type name
    #[error("cannot define type `{type_name}`: name is reserved for built-in type")]
    ReservedTypeName { type_name: String },
    /// User-defined function collides with a runtime/codegen helper symbol
    #[error("cannot define function `{function_name}`: name is reserved for a runtime helper")]
    ReservedFunctionName { function_name: String },
    /// Duplicate type definition
    #[error("duplicate type definition: `{type_name}` is already defined")]
    DuplicateTypeDefinition { type_name: String },
    /// Duplicate function definition
    #[error("duplicate function definition: `{function_name}` is already defined")]
    DuplicateFunctionDefinition { function_name: String },
    /// Linear value was not consumed before going out of scope
    #[error("linear value '{0}' must be consumed but was dropped")]
    LinearValueNotConsumed(String),
    /// Linear value was consumed on some control-flow paths but not all of them
    #[error("linear value '{0}' is not consumed on all paths")]
    LinearValueNotConsumedOnAllPaths(String),
    /// A discarded expression value (e.g. an expression statement or a loop
    /// body result) carries a linear value, which would be implicitly dropped
    #[error("discarded value of type '{type_name}' carries a linear value and must be consumed")]
    LinearValueDiscarded { type_name: String },
    /// Assignment to a place holding a live linear value (RUE-387): the
    /// overwrite would implicitly drop the old linear value.
    #[error("assignment would overwrite a live linear value of type '{type_name}'")]
    LinearValueOverwritten { type_name: String },
    /// Assignment to an `inout` parameter whose type carries a linear value
    /// (RUE-387): reassigning it would implicitly drop the caller's value.
    #[error(
        "assignment to `inout` parameter would overwrite the caller's live linear value of type '{type_name}'"
    )]
    LinearValueOverwrittenThroughInout { type_name: String },
    /// Linear struct cannot be marked @copy
    #[error("linear struct '{0}' cannot be marked @copy")]
    LinearStructCopy(String),
    /// Field access destructures a linear struct, implicitly dropping another
    /// field that carries a linear value
    #[error(
        "accessing field '{accessed}' of linear struct '{struct_name}' would implicitly drop linear field '{dropped}'",
        struct_name = .0.struct_name,
        accessed = .0.accessed,
        dropped = .0.dropped
    )]
    LinearFieldDroppedByDestructure(Box<LinearFieldDroppedByDestructureError>),
    /// Duplicate method definition in impl blocks for the same type
    #[error("duplicate method '{method_name}' for type '{type_name}'")]
    DuplicateMethod {
        type_name: String,
        method_name: String,
    },
    /// A function or method parameter list names the same parameter twice.
    /// The span points at the second (duplicate) occurrence (RUE-349).
    #[error("duplicate parameter name '{name}'")]
    DuplicateParameter { name: String },
    /// Method not found on a type
    #[error("no method named '{method_name}' found for type '{type_name}'")]
    UndefinedMethod {
        type_name: String,
        method_name: String,
    },
    /// Associated function not found on a type
    #[error("no associated function named '{function_name}' found for type '{type_name}'")]
    UndefinedAssocFn {
        type_name: String,
        function_name: String,
    },
    /// Method call on non-struct type
    #[error("no method named '{method_name}' on type '{found}'")]
    MethodCallOnNonStruct { found: String, method_name: String },
    /// Calling a method (with self) as an associated function
    #[error(
        "'{type_name}.{method_name}' is a method, not an associated function; call it on a receiver, e.g. `receiver.{method_name}()`"
    )]
    MethodCalledAsAssocFn {
        type_name: String,
        method_name: String,
    },
    /// Calling an associated function (without self) as a method
    #[error(
        "'{function_name}' is an associated function, not a method; call it on the type, e.g. `{type_name}.{function_name}()`"
    )]
    AssocFnCalledAsMethod {
        type_name: String,
        function_name: String,
    },

    // Destructor errors
    /// Duplicate destructor for the same type
    #[error("duplicate destructor for type '{type_name}'")]
    DuplicateDestructor { type_name: String },
    /// Destructor for unknown type
    #[error("unknown type '{type_name}' in destructor")]
    DestructorUnknownType { type_name: String },

    // Constant errors
    /// Duplicate constant declaration
    #[error("duplicate {kind} '{name}'")]
    DuplicateConstant { name: String, kind: String },
    /// Expression not supported in const context
    #[error("{expr_kind} is not supported in const context")]
    ConstExprNotSupported { expr_kind: String },
    /// A constant initializer (transitively) refers back to the constant
    /// being defined, so no evaluation order exists.
    #[error("cycle detected in constant initializers: {cycle}")]
    ConstInitializerCycle { cycle: String },
    /// A struct or enum (transitively) contains itself by value with no
    /// pointer indirection, so it has infinite size and no valid layout
    /// (RUE-264). `cycle` names the containment path (e.g. `A -> B -> A`).
    #[error(
        "recursive type '{name}' has infinite size (contains itself by value: {cycle}); break the cycle with a pointer (`ptr const {name}` / `ptr mut {name}`)"
    )]
    RecursiveTypeInfiniteSize { name: String, cycle: String },
    /// A value constant declared without a type annotation. Annotations are
    /// required on value constants (spec 6.5:4, RUE-179); only module
    /// bindings (`const m = @import(...)` and aliases) are exempt.
    #[error("missing type annotation on constant '{name}'")]
    ConstMissingTypeAnnotation { name: String },

    // Enum errors
    #[error("duplicate variant '{variant_name}' in enum '{enum_name}'")]
    DuplicateVariant {
        enum_name: String,
        variant_name: String,
    },
    #[error("unknown variant '{variant_name}' in enum '{enum_name}'")]
    UnknownVariant {
        enum_name: String,
        variant_name: String,
    },
    #[error("unknown enum type '{0}'")]
    UnknownEnumType(String),
    #[error("field access on non-struct type '{found}'")]
    FieldAccessOnNonStruct { found: String },
    #[error("invalid assignment target")]
    InvalidAssignmentTarget,
    /// `@raw`/`@raw_mut` operand is not an addressable place
    #[error(
        "@raw requires an addressable place (a variable, field, or array element), not a temporary value"
    )]
    RawRequiresPlace,
    /// `@field_ptr` operand is not a field-access expression `s.field`
    #[error("@field_ptr requires a field access expression (for example `s.field`)")]
    FieldPtrRequiresField,
    /// A string literal does not fit the fixed-capacity string `Str(N)` it is
    /// assigned to: its UTF-8 byte length exceeds `N` (ADR-0043 Phase 5,
    /// RUE-326).
    #[error(
        "string literal of {byte_len} bytes does not fit in `Str({capacity})` (capacity {capacity} bytes)"
    )]
    StrFixedCapacityExceeded { capacity: u64, byte_len: u64 },
    /// A string buffer (`StrBuf`/`Str(N)`) flowed into a first-class `str`
    /// slot (ADR-0043 two-types model, RUE-386). `site` names the position
    /// ("as a parameter argument", "in a binding", "as a return value", "in a
    /// struct field").
    #[error("a string buffer (`{found}`) cannot be used as a first-class `str` {site}")]
    BufferNotFirstClassStr { found: String, site: String },
    /// A first-class / static `str` value was passed as an `inout str` operand
    /// (ADR-0043 two-types model, RUE-386).
    #[error("a first-class `str` value cannot be passed as `inout str`")]
    InoutStrRequiresLocalBuffer,
    /// A whole value was assigned to an `inout str` parameter. The parameter
    /// is a second-class view over caller-owned bytes, so rebinding its header
    /// would overwrite storage whose concrete buffer representation is not
    /// `str` (RUE-641).
    #[error("an `inout str` view cannot be reassigned as a whole value")]
    StrViewReassignment,
    /// A borrowed `str` view (a `borrow`/`inout str` parameter) escaped into a
    /// first-class `str` slot (ADR-0043 two-types model, RUE-386). `site`
    /// names the position, as in [`Self::BufferNotFirstClassStr`].
    #[error("a borrowed `str` view cannot be used as a first-class `str` {site}")]
    StrViewNotFirstClass { site: String },
    /// An owning growable container (`ArrayBuf(T)`) was instantiated with a
    /// `linear` element type, via `@require_droppable` (RUE-388). The container
    /// cannot yet track element linearity, so a linear element would be leaked
    /// (never consumed); the instantiation is rejected.
    #[error(
        "`@require_droppable` requires a trivially-droppable type, but `{ty}` is `linear` — an owning growable container (e.g. `ArrayBuf`) cannot yet track element linearity, so the element would be leaked (RUE-388)"
    )]
    ContainerElementIsLinear { ty: String },
    /// A by-copy element read (`ArrayBuf(T)::get`/`get_or`) was attempted on an
    /// element type that owns resources — one with drop glue (a destructor, or a
    /// field/payload that has one). Copying such an element by `@ptr_read` leaves
    /// the copy and the still-live slot both pointing at the same owned buffer, so
    /// both run drop glue at scope exit: a double-free (RUE-651). The read is
    /// rejected; the element must be *moved* out with `pop`/`pop_or` instead (one
    /// owner) until borrow-returning reads exist. Mirrors Swift's rule that a
    /// non-copyable element cannot use a by-value `get` subscript.
    #[error(
        "cannot read an element of type `{ty}` by copy: it owns resources (has drop glue), so a by-copy read would alias the owned value and double-free it at scope exit — move it out with `pop`/`pop_or` instead (RUE-651)"
    )]
    ContainerElementNotTriviallyDroppable { ty: String },
    /// Inout argument is not an lvalue (variable, field, or array element)
    #[error("inout argument must be an lvalue (variable, field, or array element)")]
    InoutNonLvalue,
    /// Same variable passed to multiple inout parameters in a single call
    #[error("cannot pass same variable '{variable}' to multiple inout parameters")]
    InoutExclusiveAccess { variable: String },
    /// A shared by-reference position that requires an existing place and has
    /// no elaboration (RUE-953): a method's `borrow self` receiver, an
    /// accessor's receiver, and the array source of a slice coercion. An
    /// explicit `borrow` *argument* that names no place is elaborated into a
    /// promoted static or a hidden temporary instead of reaching this.
    #[error("borrow argument must be a variable, field, or array element")]
    BorrowNonLvalue,
    /// Cannot mutate a borrowed value
    #[error("cannot mutate borrowed value '{variable}'")]
    MutateBorrowedValue { variable: String },
    /// Cannot move out of a borrowed value
    #[error("cannot move out of borrowed value '{variable}'")]
    MoveOutOfBorrow { variable: String },
    /// Same variable passed to both borrow and inout parameters (law of exclusivity)
    #[error("cannot borrow '{variable}' while it is mutably borrowed (inout)")]
    BorrowInoutConflict { variable: String },
    /// A moving (by-value) use of a variable inside an argument list that also
    /// passes the same variable `inout`/`borrow`: the loan spans the whole
    /// call, so the move would leave it aliasing moved-from storage — a double
    /// free in safe code (law of exclusivity, RUE-523).
    #[error("cannot move '{variable}' into a call that also passes it '{loan_mode}'")]
    MoveWhileCallLoaned { variable: String, loan_mode: String },
    /// Argument to inout parameter is missing `inout` keyword at call site
    #[error("argument to inout parameter must use 'inout' keyword")]
    InoutKeywordMissing,
    /// Argument to borrow parameter is missing `borrow` keyword at call site
    #[error("argument to borrow parameter must use 'borrow' keyword")]
    BorrowKeywordMissing,
    /// An explicitly-mode-marked argument targets an ordinary unmarked
    /// parameter. Source argument modes must match parameter modes exactly.
    #[error("unexpected `{mode}` argument: the corresponding parameter is unmarked")]
    UnexpectedCallArgumentMode { mode: &'static str },
    /// Cannot move a value out of an inout parameter (would leave the caller's
    /// variable moved-from)
    #[error("cannot move out of inout parameter '{variable}'")]
    MoveOutOfInout { variable: String },
    /// An accessor result was returned from the enclosing function
    /// (ADR-0062: the borrowed place is scoped to its full expression).
    #[error(
        "cannot return an accessor result: `{method}` yields a second-class borrow of `{root}`, valid only within the calling expression"
    )]
    AccessorResultReturned { method: String, root: String },
    /// An accessor result was assigned into a variable, field, or element.
    #[error(
        "cannot store an accessor result: `{method}` yields a second-class borrow of `{root}`, valid only within the calling expression"
    )]
    AccessorResultStored { method: String, root: String },
    /// An accessor result was bound by a plain `let`.
    #[error(
        "cannot bind an accessor result with `let`: `{method}` yields a second-class borrow of `{root}`, valid only within the calling expression"
    )]
    AccessorResultBound { method: String, root: String },
    /// An accessor result was captured into a struct or array literal.
    #[error(
        "cannot capture an accessor result in an aggregate: `{method}` yields a second-class borrow of `{root}`, valid only within the calling expression"
    )]
    AccessorResultCaptured { method: String, root: String },
    /// An accessor body's non-diverging control flow does not end in the
    /// single trailing `yield` (ADR-0062 phase 1).
    #[error(
        "an accessor body must end in a single trailing `yield`: every non-diverging path must fall through to it, and no code may follow it"
    )]
    AccessorBodyMissingYield,
    /// An accessor body contains an exit that bypasses its trailing `yield`:
    /// a second `yield`, a `return`, or a `?` (ADR-0062 phase 1). Shares
    /// E0254 with [`ErrorKind::AccessorBodyMissingYield`]; the two halves of
    /// 6.6:6 differ only in which way the body fails to end in exactly one
    /// `yield`.
    #[error(
        "an accessor body must end in a single trailing `yield`, but this body contains {found}: guards may only diverge or fall through to the trailing `yield`"
    )]
    AccessorBodyOtherExit { found: String },
    /// The operand of an accessor's `yield` is not a place rooted at the
    /// receiver parameter.
    #[error("an accessor must yield a place rooted at `self`, not {found}")]
    AccessorYieldNotReceiverRooted { found: String },
    /// A `yield` expression outside an accessor body.
    #[error("`yield` is only valid inside the body of a `-> borrow` accessor")]
    YieldOutsideAccessor,
    /// A `-> borrow T` result on a declaration that is not a `borrow self`
    /// method.
    #[error("a `-> borrow` accessor requires a `borrow self` receiver, but this is {found}")]
    AccessorRequiresBorrowSelf { found: String },
    /// A drop-glue value read out of an accessor result by value.
    #[error(
        "cannot copy a value of type `{ty}` out of an accessor result: it owns resources (has drop glue), and the result is a borrow, not an owner"
    )]
    AccessorResultMoved { ty: String },
    /// Exclusive use of a root while an accessor result borrows it in the
    /// same full expression.
    #[error(
        "cannot use '{variable}' {conflict} while an accessor result borrows it in the same expression"
    )]
    AccessorLoanConflict {
        variable: String,
        conflict: &'static str,
    },
    /// An accessor parameter with a non-by-value mode.
    #[error(
        "an accessor parameter must be by-value: `{mode}` accessor parameters are not supported"
    )]
    AccessorParamModeUnsupported { mode: String },
    /// An accessor call re-entered an accessor already being expanded.
    #[error(
        "recursive accessor `{method}`: an accessor may not invoke itself, directly or through other accessors, in its own body"
    )]
    AccessorRecursion { method: String },
    /// Cannot move `self` out of a destructor body (RUE-139). The compiler
    /// drops a value by running its destructor and THEN dropping its fields;
    /// moving `self` to a new owner (a call argument, another binding, ...)
    /// would make that owner drop it again — re-entering the destructor in
    /// infinite recursion.
    #[error(
        "cannot move `self` out of the destructor for '{type_name}': the new owner would drop it again, re-entering this destructor"
    )]
    MoveSelfOutOfDestructor { type_name: String },
    /// Cannot move a field out of a value whose struct type has a
    /// user-defined destructor (RUE-158, the spirit of Rust's E0509).
    /// The destructor always runs on the whole value when it is dropped:
    /// it would observe the moved-out field (use-after-free for heap
    /// fields), and the automatic field cleanup after the destructor would
    /// drop the field a second time.
    #[error(
        "cannot move field `{field_name}` out of a value of type '{struct_name}', which has a destructor"
    )]
    MoveFieldOutOfDestructorType {
        struct_name: String,
        field_name: String,
    },
    /// A `@copy` struct cannot have a user-defined destructor (RUE-159, the
    /// spirit of Rust's E0184). Copies are implicit and untracked, so each
    /// copy would run the destructor again — double cleanup of the same
    /// logical resource.
    #[error("cannot define a destructor for '{type_name}': `@copy` types cannot have destructors")]
    CopyStructWithDestructor { type_name: String },

    // Control flow errors
    #[error("'break' outside of loop")]
    BreakOutsideLoop,
    #[error("'continue' outside of loop")]
    ContinueOutsideLoop,
    /// `break` with a value operand (e.g. `break 42`); break does not carry a value
    #[error("'break' with a value is not supported")]
    BreakWithValue,
    /// The `?` operator used in a function that does not return an `Option`.
    #[error(
        "the `?` operator can only be used in a function that returns an `Option` (found return type `{return_type}`)"
    )]
    QuestionOutsideOptionFn { return_type: String },
    /// The `?` operator applied to a value that is neither an `Option` nor a `Result`.
    #[error("the `?` operator can only be applied to an `Option` or `Result` (found `{found}`)")]
    QuestionOnNonOption { found: String },
    /// `?` on a `Result` in a function that does not return a `Result` (ADR-0038).
    #[error(
        "the `?` operator on a `Result` requires the enclosing function to return a `Result` (found return type `{return_type}`)"
    )]
    QuestionOutsideResultFn { return_type: String },
    /// `?` on a `Result` whose error type differs from the function's error type.
    /// Rue has no error conversion (no `From`/`Try`) until traits exist, so the
    /// error types must match exactly (ADR-0038).
    #[error(
        "the `?` operator requires matching error types: the operand is `Err({operand_err})` but the function returns `Err({fn_err})` (no error conversion until traits exist)"
    )]
    QuestionErrTypeMismatch { operand_err: String, fn_err: String },

    // Match errors
    #[error("match is not exhaustive")]
    NonExhaustiveMatch,
    #[error("match expression has no arms")]
    EmptyMatch,
    #[error("cannot match on type '{0}', expected integer, bool, or enum")]
    InvalidMatchType(String),

    // Intrinsic errors
    #[error("unknown intrinsic '@{0}'")]
    UnknownIntrinsic(String),
    #[error("{}", format_intrinsic_arg_count(name, *.expected, *.found))]
    IntrinsicWrongArgCount {
        name: String,
        expected: usize,
        found: usize,
    },
    #[error("intrinsic '@{name}' expects {expected}, found {found}", name = .0.name, expected = .0.expected, found = .0.found)]
    IntrinsicTypeMismatch(Box<IntrinsicTypeMismatchError>),
    #[error(
        "cannot infer the target type of '@{0}'; add a type annotation \
         (e.g. `let n: i32 = @{0}(...)`) or use it where a specific integer \
         type is expected"
    )]
    CannotInferCastTarget(String),
    /// `@bitCast` between integer types of different widths (RUE-952).
    /// Reinterpretation is width-preserving by construction, so the diagnostic
    /// names both widths and points at `@intCast` for the value-changing
    /// conversion.
    #[error(
        "'@bitCast' requires the source and target to have the same width, but \
         `{from}` is {from_bits}-bit and `{to}` is {to_bits}-bit"
    )]
    BitCastWidthMismatch {
        from: String,
        to: String,
        from_bits: u32,
        to_bits: u32,
    },
    #[error(
        "cannot infer the pointee type of '@{0}'; add a type annotation \
         (e.g. `let p: ptr mut T = @{0}(...)`) or use it where a specific \
         `ptr mut T` is expected"
    )]
    CannotInferPointeeType(String),
    #[error(
        "the `align` argument to '@{name}' must be a power of two, but a \
         constant value of {value} was given"
    )]
    IntrinsicAlignNotPowerOfTwo { name: String, value: u64 },

    // Module errors
    #[error("@import requires a string literal argument")]
    ImportRequiresStringLiteral,
    #[error("cannot find module '{path}'")]
    ModuleNotFound {
        path: String,
        /// Candidates that were tried (for error message)
        candidates: Vec<String>,
    },
    #[error("ambiguous module '{}': both '{}' and '{}' exist", .0.path, .0.file_module, .0.dir_module)]
    AmbiguousModule(Box<AmbiguousModuleData>),
    #[error("standard library not found")]
    StdLibNotFound,
    #[error("{item_kind} `{name}` is private")]
    PrivateMemberAccess { item_kind: String, name: String },
    #[error("{} `{}` is private (defined in `{}`)", .0.item_kind, .0.name, .0.defining_file)]
    PrivateUnqualifiedAccess(Box<PrivateUnqualifiedAccessData>),
    #[error("module `{module_name}` has no member `{member_name}`")]
    UnknownModuleMember {
        module_name: String,
        member_name: String,
    },

    // Literal errors
    #[error("literal value {value} is out of range for type '{ty}'")]
    LiteralOutOfRange { value: u64, ty: String },

    // Operator errors
    #[error("cannot apply unary operator `-` to type '{0}'")]
    CannotNegate(String),
    #[error("comparison operators cannot be chained")]
    ChainedComparison,

    // Array errors
    #[error("cannot index into non-array type '{found}'")]
    IndexOnNonArray { found: String },
    #[error("{}", format_array_length_mismatch(*.expected, *.found))]
    ArrayLengthMismatch { expected: u64, found: u64 },
    // `index` is i128 so a constant u64 index above i64::MAX is still
    // reported exactly (RUE-532) — the old i64 narrowing made such an index
    // look non-constant and skip the compile-time bounds check entirely.
    #[error("index out of bounds: the length is {length} but the index is {index}")]
    IndexOutOfBounds { index: i128, length: u64 },
    #[error("type annotation required for empty array")]
    TypeAnnotationRequired,
    /// Cannot move non-Copy element out of array index position.
    /// Raised only for moves the per-element tracker cannot follow: dynamic
    /// (non-constant) indices, and indexing that is not rooted directly at an
    /// array variable. A constant index into an array variable moves just
    /// that element out (spec 3.8:68, RUE-186).
    #[error("cannot move out of indexed position: element type '{element_type}' is not Copy")]
    MoveOutOfIndex { element_type: String },
    /// Array-repeat literal `[value; count]` whose element type is not Copy.
    /// A repeat literal materializes `count` copies of a single value, so the
    /// element type must be Copy (matching Rust's `[v; N]: Copy` requirement,
    /// RUE-235).
    #[error("array-repeat literal requires a Copy element type, but '{element_type}' is not Copy")]
    ArrayRepeatNonCopy { element_type: String },
    /// A type's layout exceeds the implementation's maximum object size
    /// (Appendix C practical limit; RUE-561 — unchecked u32 slot arithmetic
    /// previously ICEd on 2 GiB arrays and silently truncated 32 GiB ones to
    /// zero-sized).
    ///
    /// The limit that is actually enforced is a count of 8-byte ABI slots, not
    /// a byte total: a layout spends one slot per scalar, per struct field, and
    /// per array element, whatever the element's own width. The message names
    /// the slot ceiling so it reports the limit the compiler checks, as spec
    /// C.1:2 requires (RUE-1272).
    #[error(
        "type '{type_name}' exceeds the maximum supported object size \
         ({max_slots} ABI slots; a layout spends one 8-byte slot per scalar, \
         struct field, and array element)"
    )]
    TypeTooLarge { type_name: String, max_slots: u64 },
    /// A function's cumulative locals, parameter homes, sret cell, spills, or
    /// transient outgoing call area exceeds the backend displacement budget.
    #[error(
        "function stack frame or outgoing call area exceeds the maximum supported size \
         ({max_bytes} bytes)"
    )]
    FunctionFrameTooLarge { max_bytes: u64 },
    /// Assignment into an array (element write, or a write through an element)
    /// while one or more of its elements are moved out (RUE-186). Reinstating
    /// per-element ownership through writes is not supported; the whole array
    /// must be reinitialized instead.
    #[error("cannot assign into '{array}': one of its elements has been moved out")]
    AssignToPartiallyMovedArray { array: String },

    // Linker errors
    #[error("link error: {0}")]
    LinkError(String),

    // Target errors
    #[error("unsupported target: {0}")]
    UnsupportedTarget(String),

    // Preview feature errors
    #[error("{what} requires preview feature `{}`", .feature.name())]
    PreviewFeatureRequired {
        feature: PreviewFeature,
        what: String,
    },

    /// A type appeared in an `extern "C"` signature that the current FFI phase
    /// cannot classify. C FFI P3 (ADR-0064, RUE-1057) supports every integer and
    /// pointer scalar (`i8`/`u8`/`i16`/`u16`/`i32`/`u32`/`i64`/`u64`, `bool` as
    /// `_Bool`, and raw pointers) *and* C-classifiable `@repr(c)` aggregates.
    /// What remains rejected here: a Rue enum (not FFI-safe in v0), and — until
    /// RUE-714 adds the type (P5) — floating-point.
    #[error(
        "type `{ty}` is not supported in an `extern \"C\"` signature: \
         C FFI (ADR-0064) supports integer and pointer scalars and \
         C-classifiable `@repr(c)` aggregates — enums are not FFI-safe and \
         floating-point awaits RUE-714"
    )]
    ExternSignatureTypeUnsupported {
        /// The rejected type, as rendered for the user.
        ty: String,
    },

    /// An aggregate type appeared in an `extern "C"` signature without the
    /// `@repr(c)` guarantee marker (ADR-0064 Amendment 1). The marker is the
    /// guarantee trigger: any aggregate crossing the C boundary must opt in
    /// explicitly, never be silently promoted to C layout.
    #[error(
        "aggregate type `{ty}` in an `extern \"C\"` signature must be marked \
         `@repr(c)`: aggregates crossing the C boundary opt in to C layout \
         explicitly (ADR-0064)"
    )]
    ExternAggregateNotReprC {
        /// The unmarked aggregate type, as rendered for the user.
        ty: String,
    },

    /// A fixed-size array appeared directly as an `extern "C"` parameter or
    /// return type (ADR-0064 Amendment 1). C decays an array argument to a
    /// pointer and has no by-value array parameter, so a fixed array is only
    /// eligible as a struct *field*, never as a direct signature type.
    #[error(
        "fixed-size array `{ty}` cannot appear directly in an `extern \"C\"` \
         signature: C decays arrays to pointers — pass a pointer (`ptr const T`) \
         instead, or wrap the array in a `@repr(c)` struct (ADR-0064)"
    )]
    ExternArrayByValue {
        /// The rejected array type, as rendered for the user.
        ty: String,
    },

    /// A C variadic marker (`...`) appeared in an `extern "C"` parameter list.
    /// Variadic foreign calls are rejected in v0 (ADR-0064 secondary ruling B,
    /// P6): the target-C classifier implements a fixed-signature calling
    /// convention only, and matching C's variadic argument-promotion and
    /// register-save-area contract is a later, separate design. The parser
    /// recognizes the `...` token specifically so the boundary reports this
    /// rather than a generic "unexpected token".
    #[error(
        "variadic parameters (`...`) are not supported in an `extern \"C\"` \
         signature: C variadic calls are rejected in v0 (ADR-0064)"
    )]
    ExternVariadicUnsupported,

    /// A `pub extern "C" fn` Rue-to-C export (ADR-0064 P4) has a signature the
    /// export-thunk lowering cannot bridge yet. The P4 export thunk marshals
    /// only integer/pointer scalars that fit the target's argument-register
    /// budget: an aggregate parameter or return, or a scalar parameter list
    /// wider than the register budget, is rejected here rather than silently
    /// mis-marshaled, and an export whose C name collides with the program
    /// entry point (`main`) is rejected to keep the entry symbol unambiguous.
    #[error("`pub extern \"C\" fn` export `{name}` is not supported: {reason} (ADR-0064 P4)")]
    ExportSignatureUnsupported {
        /// The export's C symbol name.
        name: String,
        /// Why the export cannot be lowered, phrased for the user.
        reason: String,
    },

    /// Two `extern "C"` foreign declarations name the same C symbol with
    /// disagreeing Rue signatures (RUE-1218, spec 9.3:5). A foreign declaration
    /// is a description of an *external* function, not a Rue definition, so its
    /// internal symbol is the raw C name it declares (RUE-1125) — two modules
    /// declaring the same symbol produce one undefined symbol for the linker.
    /// Identical redeclarations are legal, matching C; disagreeing ones cannot
    /// both describe the definition that will be linked in, and without this
    /// rule the last-collected signature silently wins for every call site.
    #[error(
        "`extern \"C\"` symbol `{}` is declared with conflicting signatures: `{}` here, \
         but `{}` earlier",
        .0.symbol,
        .0.declared,
        .0.previously_declared
    )]
    ForeignSignatureConflict(Box<ForeignSignatureConflictError>),

    /// An `extern "C"` foreign declaration names `main` (RUE-1220, spec
    /// 9.3:6). The declared C symbol is the program's own entry point, which
    /// belongs to the runtime start glue (`_start`/`__main`, spec 6.1:38), so
    /// there is no external `main` for such a declaration to describe: in a
    /// non-root module it silently binds the program's own entry point and a
    /// call through it recurses, and in the root module it collides with the
    /// entry point's definition. Rejected in every module and for every
    /// signature, mirroring E1106's rejection of an export named `main`.
    #[error(
        "`extern \"C\"` declaration of `main` names the program entry point, which is not an \
         external function (ADR-0064)"
    )]
    ForeignEntryPointDeclaration,

    /// A `@repr(c)` struct failed the reject-don't-guess eligibility check
    /// (ADR-0064 Amendment 1): an empty struct, an enum/aggregate field without
    /// its own `@repr(c)` marker, or a linear / destructor-bearing field. The
    /// marker is rejected rather than guessing a C representation.
    #[error("`@repr(c)` struct `{}` is not FFI-eligible: {}", .0.struct_name, .0.reason)]
    ReprCStructIneligible(Box<ReprCIneligibleError>),

    /// A slice type / range-slice was used under `--preview slices` but the
    /// fat-pointer runtime is not implemented yet (ADR-0043 Phase 1, RUE-322).
    #[error("slice types are not yet fully implemented (ADR-0043 Phase 1, RUE-322)")]
    SliceNotYetImplemented,

    /// A float literal was written with `--preview floats` enabled, but the
    /// phases that give it a type do not exist yet (ADR-0065 Phase 4+,
    /// RUE-714). Phases 2 and 3 land the literal token, the parser node, and
    /// the untyped RIR node; semantic analysis has no `f32`/`f64` tag and no
    /// `comptime_float` to coerce it with, so the literal is rejected here
    /// with a clean diagnostic rather than reaching an unfinished typing path.
    /// Modelled on [`ErrorKind::SliceNotYetImplemented`], the same
    /// gate-is-on-but-phase-is-missing marker for ADR-0043.
    #[error(
        "floating-point literals are not yet supported at this compilation phase \
         (ADR-0065 Phase 4, RUE-714)"
    )]
    FloatNotYetImplemented,

    /// A slice type `[T]` appeared in return position — forbidden because a
    /// slice is second-class (ADR-0037, ADR-0043, RUE-322).
    #[error(
        "a slice type `[T]` cannot be returned: slices are second-class views \
         valid only in argument position (ADR-0043)"
    )]
    SliceReturnNotAllowed,

    /// A slice type `[T]` appeared as a struct field type — forbidden because
    /// a slice is second-class (ADR-0037, ADR-0043, RUE-322).
    #[error(
        "a slice type `[T]` cannot be stored in a struct field: slices are \
         second-class views valid only in argument position (ADR-0043)"
    )]
    SliceInAggregateField,

    /// A slice type `[T]` appeared in a `let` or `const` binding — forbidden
    /// because a slice is second-class (ADR-0037, ADR-0043, RUE-322).
    #[error(
        "a slice type `[T]` can only name a function parameter: slices are \
         second-class views and cannot be bound past their argument scope (ADR-0043)"
    )]
    SliceEscapesScope,

    // Comptime errors
    #[error("comptime evaluation failed: {reason}")]
    ComptimeEvaluationFailed { reason: String },

    #[error("comptime parameter requires a compile-time known value")]
    ComptimeArgNotConst { param_name: String },

    // Unchecked-code errors
    #[error("{what} requires a `checked` block")]
    UncheckedOpRequiresChecked { what: String },

    // Compiler-input errors
    #[error("invalid compiler input: {0}")]
    InvalidCompilerInput(String),

    /// A source-driven request exceeded a documented bounded compiler
    /// representation (spec Appendix C). This is a normal diagnostic, not an
    /// ICE: spec C.1:2 requires a diagnosable compile-time failure rather than
    /// a wrapped index or an abnormal termination.
    #[error("compiler resource limit exceeded: {0}")]
    CompilerResourceLimit(String),

    /// The compiler could not acquire memory for an otherwise valid request.
    /// This is a normal environmental failure, not an ICE.
    #[error("compiler resource exhaustion: {0}")]
    CompilerResourceExhaustion(String),

    /// The compiled executable could not be published to the output path
    /// (write, permission, signing, or rename failure). Publication is
    /// atomic: on this error no partial output artifact remains (RUE-781).
    #[error("output publication failed: {0}")]
    OutputPublication(String),

    /// A required trusted toolchain input — a standard-library module the
    /// program's reached bodies demand (RUE-1112) — was absent from the
    /// compilation and the caller did not supply it. The standard library is a
    /// toolchain guarantee, so a stable no-filesystem entry that observes an
    /// unsatisfied demand reports this deterministic contract failure at its
    /// boundary rather than an ICE: the CLI host acquires the module and retries,
    /// while an embedder that omits a guaranteed toolchain input gets a clear,
    /// distinguishable contract error.
    #[error("unsatisfied trusted toolchain input: {0}")]
    UnsatisfiedTrustedToolchainInput(String),

    // Internal compiler errors (bugs in the compiler itself)
    #[error("internal compiler producer invariant: {0}")]
    CompilerProducerInvariant(String),
    #[error("internal compiler error: {0}")]
    InternalError(String),

    // Codegen internal errors (compiler bugs)
    #[error("internal codegen error: {0}")]
    InternalCodegenError(String),
}

impl ErrorKind {
    /// Get the error code for this error kind.
    ///
    /// Every error kind has a unique, stable error code that can be used
    /// for documentation lookup and searchability.
    pub fn code(&self) -> ErrorCode {
        match self {
            // Lexer errors (E0001-E0099)
            ErrorKind::UnexpectedCharacter(_) => ErrorCode::UNEXPECTED_CHARACTER,
            ErrorKind::InvalidInteger => ErrorCode::INVALID_INTEGER,
            ErrorKind::InvalidStringEscape(_) => ErrorCode::INVALID_STRING_ESCAPE,
            ErrorKind::UnterminatedString => ErrorCode::UNTERMINATED_STRING,
            ErrorKind::UppercaseBasePrefix(_) => ErrorCode::UPPERCASE_BASE_PREFIX,
            ErrorKind::EmptyBasedLiteral { .. } => ErrorCode::EMPTY_BASED_LITERAL,
            ErrorKind::MalformedByteLiteral(_) => ErrorCode::MALFORMED_BYTE_LITERAL,
            ErrorKind::MalformedFloatLiteral(_) => ErrorCode::MALFORMED_FLOAT_LITERAL,
            ErrorKind::InvalidDigitForBase { .. } => ErrorCode::INVALID_DIGIT_FOR_BASE,
            ErrorKind::LexerDiagnosticsOmitted { .. } => ErrorCode::LEXER_DIAGNOSTICS_OMITTED,

            // Parser errors (E0100-E0199)
            ErrorKind::UnexpectedToken { .. } => ErrorCode::UNEXPECTED_TOKEN,
            ErrorKind::ParseError(_) => ErrorCode::PARSE_ERROR,
            ErrorKind::ParserDiagnosticsOmitted { .. } => ErrorCode::PARSER_DIAGNOSTICS_OMITTED,
            ErrorKind::NestingLimitExceeded { .. } => ErrorCode::NESTING_LIMIT_EXCEEDED,

            // Semantic errors (E0200-E0399)
            ErrorKind::NoMainFunction => ErrorCode::NO_MAIN_FUNCTION,
            ErrorKind::InvalidMainSignature { .. } => ErrorCode::INVALID_MAIN_SIGNATURE,
            ErrorKind::UndefinedVariable(_) => ErrorCode::UNDEFINED_VARIABLE,
            ErrorKind::UndefinedFunction(_) => ErrorCode::UNDEFINED_FUNCTION,
            ErrorKind::AssignToImmutable(_) => ErrorCode::ASSIGN_TO_IMMUTABLE,
            ErrorKind::UnknownType(_) => ErrorCode::UNKNOWN_TYPE,
            ErrorKind::InvalidArrayLength { .. } => ErrorCode::INVALID_ARRAY_LENGTH,
            ErrorKind::UseAfterMove(_) => ErrorCode::USE_AFTER_MOVE,
            ErrorKind::TypeMismatch { .. } => ErrorCode::TYPE_MISMATCH,
            ErrorKind::WrongArgumentCount { .. } => ErrorCode::WRONG_ARGUMENT_COUNT,
            ErrorKind::DuplicatePatternBinding { .. } => ErrorCode::DUPLICATE_PATTERN_BINDING,
            ErrorKind::StrViewReassignment => ErrorCode::STR_VIEW_REASSIGNMENT,

            // Struct/enum errors (E0400-E0499)
            ErrorKind::MissingFields(_) => ErrorCode::MISSING_FIELDS,
            ErrorKind::UnknownField { .. } => ErrorCode::UNKNOWN_FIELD,
            ErrorKind::DuplicateField { .. } => ErrorCode::DUPLICATE_FIELD,
            ErrorKind::EmptyStruct => ErrorCode::EMPTY_STRUCT,
            ErrorKind::CopyStructNonCopyField(_) => ErrorCode::COPY_STRUCT_NON_COPY_FIELD,
            ErrorKind::ReservedTypeName { .. } => ErrorCode::RESERVED_TYPE_NAME,
            ErrorKind::ReservedFunctionName { .. } => ErrorCode::RESERVED_FUNCTION_NAME,
            ErrorKind::DuplicateTypeDefinition { .. } => ErrorCode::DUPLICATE_TYPE_DEFINITION,
            ErrorKind::DuplicateFunctionDefinition { .. } => {
                ErrorCode::DUPLICATE_FUNCTION_DEFINITION
            }
            ErrorKind::LinearValueNotConsumed(_) => ErrorCode::LINEAR_VALUE_NOT_CONSUMED,
            ErrorKind::LinearValueNotConsumedOnAllPaths(_) => {
                ErrorCode::LINEAR_VALUE_NOT_CONSUMED_ON_ALL_PATHS
            }
            ErrorKind::LinearValueDiscarded { .. } => ErrorCode::LINEAR_VALUE_DISCARDED,
            ErrorKind::LinearValueOverwritten { .. } => ErrorCode::LINEAR_VALUE_OVERWRITTEN,
            ErrorKind::LinearValueOverwrittenThroughInout { .. } => {
                ErrorCode::LINEAR_VALUE_OVERWRITTEN_THROUGH_INOUT
            }
            ErrorKind::LinearStructCopy(_) => ErrorCode::LINEAR_STRUCT_COPY,
            ErrorKind::LinearFieldDroppedByDestructure(_) => {
                ErrorCode::LINEAR_FIELD_DROPPED_BY_DESTRUCTURE
            }
            ErrorKind::DuplicateMethod { .. } => ErrorCode::DUPLICATE_METHOD,
            ErrorKind::DuplicateParameter { .. } => ErrorCode::DUPLICATE_PARAMETER,
            ErrorKind::UndefinedMethod { .. } => ErrorCode::UNDEFINED_METHOD,
            ErrorKind::UndefinedAssocFn { .. } => ErrorCode::UNDEFINED_ASSOC_FN,
            ErrorKind::MethodCallOnNonStruct { .. } => ErrorCode::METHOD_CALL_ON_NON_STRUCT,
            ErrorKind::MethodCalledAsAssocFn { .. } => ErrorCode::METHOD_CALLED_AS_ASSOC_FN,
            ErrorKind::AssocFnCalledAsMethod { .. } => ErrorCode::ASSOC_FN_CALLED_AS_METHOD,
            ErrorKind::DuplicateDestructor { .. } => ErrorCode::DUPLICATE_DESTRUCTOR,
            ErrorKind::DestructorUnknownType { .. } => ErrorCode::DESTRUCTOR_UNKNOWN_TYPE,
            ErrorKind::DuplicateConstant { .. } => ErrorCode::DUPLICATE_CONSTANT,
            ErrorKind::ConstExprNotSupported { .. } => ErrorCode::CONST_EXPR_NOT_SUPPORTED,
            ErrorKind::ConstInitializerCycle { .. } => ErrorCode::CONST_INITIALIZER_CYCLE,
            ErrorKind::RecursiveTypeInfiniteSize { .. } => ErrorCode::RECURSIVE_TYPE_INFINITE_SIZE,
            ErrorKind::ConstMissingTypeAnnotation { .. } => {
                ErrorCode::CONST_MISSING_TYPE_ANNOTATION
            }
            ErrorKind::DuplicateVariant { .. } => ErrorCode::DUPLICATE_VARIANT,
            ErrorKind::UnknownVariant { .. } => ErrorCode::UNKNOWN_VARIANT,
            ErrorKind::UnknownEnumType(_) => ErrorCode::UNKNOWN_ENUM_TYPE,
            ErrorKind::FieldAccessOnNonStruct { .. } => ErrorCode::FIELD_ACCESS_ON_NON_STRUCT,
            ErrorKind::InvalidAssignmentTarget => ErrorCode::INVALID_ASSIGNMENT_TARGET,
            ErrorKind::RawRequiresPlace => ErrorCode::RAW_REQUIRES_PLACE,
            ErrorKind::FieldPtrRequiresField => ErrorCode::FIELD_PTR_REQUIRES_FIELD,
            ErrorKind::StrFixedCapacityExceeded { .. } => ErrorCode::STR_FIXED_CAPACITY_EXCEEDED,
            ErrorKind::BufferNotFirstClassStr { .. } => ErrorCode::BUFFER_NOT_FIRST_CLASS_STR,
            ErrorKind::InoutStrRequiresLocalBuffer => ErrorCode::INOUT_STR_REQUIRES_LOCAL_BUFFER,
            ErrorKind::StrViewNotFirstClass { .. } => ErrorCode::STR_VIEW_NOT_FIRST_CLASS,
            ErrorKind::ContainerElementIsLinear { .. } => ErrorCode::CONTAINER_ELEMENT_IS_LINEAR,
            ErrorKind::InoutNonLvalue => ErrorCode::INOUT_NON_LVALUE,
            ErrorKind::InoutExclusiveAccess { .. } => ErrorCode::INOUT_EXCLUSIVE_ACCESS,
            ErrorKind::BorrowNonLvalue => ErrorCode::BORROW_NON_LVALUE,
            ErrorKind::MutateBorrowedValue { .. } => ErrorCode::MUTATE_BORROWED_VALUE,
            ErrorKind::MoveOutOfBorrow { .. } => ErrorCode::MOVE_OUT_OF_BORROW,
            ErrorKind::BorrowInoutConflict { .. } => ErrorCode::BORROW_INOUT_CONFLICT,
            ErrorKind::MoveWhileCallLoaned { .. } => ErrorCode::MOVE_WHILE_CALL_LOANED,
            ErrorKind::AccessorResultReturned { .. } => ErrorCode::ACCESSOR_RESULT_RETURNED,
            ErrorKind::AccessorResultStored { .. } => ErrorCode::ACCESSOR_RESULT_STORED,
            ErrorKind::AccessorResultBound { .. } => ErrorCode::ACCESSOR_RESULT_BOUND,
            ErrorKind::AccessorResultCaptured { .. } => ErrorCode::ACCESSOR_RESULT_CAPTURED,
            ErrorKind::AccessorBodyMissingYield | ErrorKind::AccessorBodyOtherExit { .. } => {
                ErrorCode::ACCESSOR_BODY_MISSING_YIELD
            }
            ErrorKind::AccessorYieldNotReceiverRooted { .. } => {
                ErrorCode::ACCESSOR_YIELD_NOT_RECEIVER_ROOTED
            }
            ErrorKind::YieldOutsideAccessor => ErrorCode::YIELD_OUTSIDE_ACCESSOR,
            ErrorKind::AccessorRequiresBorrowSelf { .. } => {
                ErrorCode::ACCESSOR_REQUIRES_BORROW_SELF
            }
            ErrorKind::AccessorResultMoved { .. } => ErrorCode::ACCESSOR_RESULT_MOVED,
            ErrorKind::AccessorLoanConflict { .. } => ErrorCode::ACCESSOR_LOAN_CONFLICT,
            ErrorKind::AccessorParamModeUnsupported { .. } => {
                ErrorCode::ACCESSOR_PARAM_MODE_UNSUPPORTED
            }
            ErrorKind::AccessorRecursion { .. } => ErrorCode::ACCESSOR_RECURSION,
            ErrorKind::InoutKeywordMissing => ErrorCode::INOUT_KEYWORD_MISSING,
            ErrorKind::BorrowKeywordMissing => ErrorCode::BORROW_KEYWORD_MISSING,
            ErrorKind::UnexpectedCallArgumentMode { .. } => {
                ErrorCode::UNEXPECTED_CALL_ARGUMENT_MODE
            }
            ErrorKind::MoveOutOfInout { .. } => ErrorCode::MOVE_OUT_OF_INOUT,
            ErrorKind::MoveSelfOutOfDestructor { .. } => ErrorCode::MOVE_SELF_OUT_OF_DESTRUCTOR,
            ErrorKind::MoveFieldOutOfDestructorType { .. } => {
                ErrorCode::MOVE_FIELD_OUT_OF_DESTRUCTOR_TYPE
            }
            ErrorKind::CopyStructWithDestructor { .. } => ErrorCode::COPY_STRUCT_WITH_DESTRUCTOR,

            // Control flow errors (E0500-E0599)
            ErrorKind::BreakOutsideLoop => ErrorCode::BREAK_OUTSIDE_LOOP,
            ErrorKind::ContinueOutsideLoop => ErrorCode::CONTINUE_OUTSIDE_LOOP,
            ErrorKind::BreakWithValue => ErrorCode::BREAK_WITH_VALUE,
            ErrorKind::QuestionOutsideOptionFn { .. } => ErrorCode::QUESTION_OUTSIDE_OPTION_FN,
            ErrorKind::QuestionOnNonOption { .. } => ErrorCode::QUESTION_ON_NON_OPTION,
            ErrorKind::QuestionOutsideResultFn { .. } => ErrorCode::QUESTION_OUTSIDE_RESULT_FN,
            ErrorKind::QuestionErrTypeMismatch { .. } => ErrorCode::QUESTION_ERR_TYPE_MISMATCH,

            // Match errors (E0600-E0699)
            ErrorKind::NonExhaustiveMatch => ErrorCode::NON_EXHAUSTIVE_MATCH,
            ErrorKind::EmptyMatch => ErrorCode::EMPTY_MATCH,
            ErrorKind::InvalidMatchType(_) => ErrorCode::INVALID_MATCH_TYPE,

            // Intrinsic errors (E0700-E0799)
            ErrorKind::UnknownIntrinsic(_) => ErrorCode::UNKNOWN_INTRINSIC,
            ErrorKind::IntrinsicWrongArgCount { .. } => ErrorCode::INTRINSIC_WRONG_ARG_COUNT,
            ErrorKind::IntrinsicTypeMismatch(_) => ErrorCode::INTRINSIC_TYPE_MISMATCH,
            ErrorKind::CannotInferCastTarget(_) => ErrorCode::CANNOT_INFER_CAST_TARGET,
            ErrorKind::BitCastWidthMismatch { .. } => ErrorCode::BIT_CAST_WIDTH_MISMATCH,
            ErrorKind::CannotInferPointeeType(_) => ErrorCode::CANNOT_INFER_POINTEE_TYPE,
            ErrorKind::IntrinsicAlignNotPowerOfTwo { .. } => {
                ErrorCode::INTRINSIC_ALIGN_NOT_POWER_OF_TWO
            }
            ErrorKind::ContainerElementNotTriviallyDroppable { .. } => {
                ErrorCode::CONTAINER_ELEMENT_NOT_TRIVIALLY_DROPPABLE
            }
            ErrorKind::ImportRequiresStringLiteral => ErrorCode::IMPORT_REQUIRES_STRING_LITERAL,
            ErrorKind::ModuleNotFound { .. } => ErrorCode::MODULE_NOT_FOUND,
            ErrorKind::AmbiguousModule { .. } => ErrorCode::AMBIGUOUS_MODULE,
            ErrorKind::StdLibNotFound => ErrorCode::STD_LIB_NOT_FOUND,
            ErrorKind::PrivateMemberAccess { .. } => ErrorCode::PRIVATE_MEMBER_ACCESS,
            ErrorKind::PrivateUnqualifiedAccess(_) => ErrorCode::PRIVATE_UNQUALIFIED_ACCESS,
            ErrorKind::UnknownModuleMember { .. } => ErrorCode::UNKNOWN_MODULE_MEMBER,

            // Literal/operator errors (E0800-E0899)
            ErrorKind::LiteralOutOfRange { .. } => ErrorCode::LITERAL_OUT_OF_RANGE,
            ErrorKind::CannotNegate(_) => ErrorCode::CANNOT_NEGATE,
            ErrorKind::ChainedComparison => ErrorCode::CHAINED_COMPARISON,

            // Array errors (E0900-E0999)
            ErrorKind::IndexOnNonArray { .. } => ErrorCode::INDEX_ON_NON_ARRAY,
            ErrorKind::ArrayLengthMismatch { .. } => ErrorCode::ARRAY_LENGTH_MISMATCH,
            ErrorKind::IndexOutOfBounds { .. } => ErrorCode::INDEX_OUT_OF_BOUNDS,
            ErrorKind::TypeAnnotationRequired => ErrorCode::TYPE_ANNOTATION_REQUIRED,
            ErrorKind::MoveOutOfIndex { .. } => ErrorCode::MOVE_OUT_OF_INDEX,
            ErrorKind::ArrayRepeatNonCopy { .. } => ErrorCode::ARRAY_REPEAT_NON_COPY,
            ErrorKind::TypeTooLarge { .. } => ErrorCode::TYPE_TOO_LARGE,
            ErrorKind::FunctionFrameTooLarge { .. } => ErrorCode::FUNCTION_FRAME_TOO_LARGE,
            ErrorKind::AssignToPartiallyMovedArray { .. } => {
                ErrorCode::ASSIGN_TO_PARTIALLY_MOVED_ARRAY
            }

            // Linker/target errors (E1000-E1099)
            ErrorKind::LinkError(_) => ErrorCode::LINK_ERROR,
            ErrorKind::UnsupportedTarget(_) => ErrorCode::UNSUPPORTED_TARGET,

            // Preview feature errors (E1100-E1199)
            ErrorKind::PreviewFeatureRequired { .. } => ErrorCode::PREVIEW_FEATURE_REQUIRED,
            ErrorKind::ExternSignatureTypeUnsupported { .. } => {
                ErrorCode::EXTERN_SIGNATURE_TYPE_UNSUPPORTED
            }
            ErrorKind::ExternAggregateNotReprC { .. } => ErrorCode::EXTERN_AGGREGATE_NOT_REPR_C,
            ErrorKind::ExternArrayByValue { .. } => ErrorCode::EXTERN_ARRAY_BY_VALUE,
            ErrorKind::ExternVariadicUnsupported => ErrorCode::EXTERN_VARIADIC_UNSUPPORTED,
            ErrorKind::ExportSignatureUnsupported { .. } => ErrorCode::EXPORT_SIGNATURE_UNSUPPORTED,
            ErrorKind::ForeignSignatureConflict(_) => ErrorCode::FOREIGN_SIGNATURE_CONFLICT,
            ErrorKind::ForeignEntryPointDeclaration => ErrorCode::FOREIGN_ENTRY_POINT_DECLARATION,
            ErrorKind::ReprCStructIneligible(_) => ErrorCode::REPR_C_STRUCT_INELIGIBLE,
            ErrorKind::SliceNotYetImplemented => ErrorCode::SLICE_NOT_YET_IMPLEMENTED,
            ErrorKind::FloatNotYetImplemented => ErrorCode::FLOAT_NOT_YET_IMPLEMENTED,
            ErrorKind::SliceReturnNotAllowed => ErrorCode::SLICE_RETURN_NOT_ALLOWED,
            ErrorKind::SliceInAggregateField => ErrorCode::SLICE_IN_AGGREGATE_FIELD,
            ErrorKind::SliceEscapesScope => ErrorCode::SLICE_ESCAPES_SCOPE,

            // Comptime errors (E1200-E1299)
            ErrorKind::ComptimeEvaluationFailed { .. } => ErrorCode::COMPTIME_EVALUATION_FAILED,
            ErrorKind::ComptimeArgNotConst { .. } => ErrorCode::COMPTIME_ARG_NOT_CONST,

            // Unchecked-code errors (E1300-E1399)
            ErrorKind::UncheckedOpRequiresChecked { .. } => {
                ErrorCode::UNCHECKED_OP_REQUIRES_CHECKED
            }

            // Compiler-input errors (E1400-E1499)
            ErrorKind::InvalidCompilerInput(_) => ErrorCode::INVALID_COMPILER_INPUT,
            ErrorKind::CompilerResourceLimit(_) => ErrorCode::COMPILER_RESOURCE_LIMIT,
            ErrorKind::CompilerResourceExhaustion(_) => ErrorCode::COMPILER_RESOURCE_EXHAUSTION,
            ErrorKind::OutputPublication(_) => ErrorCode::OUTPUT_PUBLICATION,
            ErrorKind::UnsatisfiedTrustedToolchainInput(_) => {
                ErrorCode::UNSATISFIED_TRUSTED_TOOLCHAIN_INPUT
            }

            // Internal compiler errors (E9000-E9999)
            ErrorKind::CompilerProducerInvariant(_) | ErrorKind::InternalError(_) => {
                ErrorCode::INTERNAL_ERROR
            }
            ErrorKind::InternalCodegenError(_) => ErrorCode::INTERNAL_CODEGEN_ERROR,
        }
    }
}

/// Result type for compilation operations.
pub type CompileResult<T> = Result<T, CompileError>;

// ============================================================================
// Multiple Error Collection
// ============================================================================

/// A collection of compilation errors.
///
/// This type supports collecting multiple errors during compilation to provide
/// users with more comprehensive diagnostics. Instead of stopping at the first
/// error, the compiler can continue and report multiple issues at once.
///
/// # Usage
///
/// Use `CompileErrors` when a compilation phase can detect multiple independent
/// errors. For example, semantic analysis can report multiple type errors in
/// different functions.
///
/// ```ignore
/// let mut errors = CompileErrors::new();
/// errors.push(CompileError::new(ErrorKind::TypeMismatch { ... }, span1));
/// errors.push(CompileError::new(ErrorKind::UndefinedVariable("x".into()), span2));
///
/// if !errors.is_empty() {
///     return Err(errors);
/// }
/// ```
///
/// # Error Semantics
///
/// - An empty `CompileErrors` represents no errors (not a failure)
/// - A non-empty `CompileErrors` represents one or more compilation failures
/// - When converted to a single `CompileError`, the first error is used
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileErrors {
    errors: Vec<CompileError>,
}

impl CompileErrors {
    /// Create a new empty error collection.
    pub fn new() -> Self {
        Self { errors: Vec::new() }
    }

    /// Create an error collection from a single error.
    pub fn from_error(error: CompileError) -> Self {
        Self {
            errors: vec![error],
        }
    }

    /// Add an error to the collection.
    pub fn push(&mut self, error: CompileError) {
        self.errors.push(error);
    }

    /// Extend this collection with errors from another collection.
    pub fn extend(&mut self, other: CompileErrors) {
        self.errors.extend(other.errors);
    }

    /// Returns true if there are no errors.
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Returns the number of errors.
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Get the first error, if any.
    pub fn first(&self) -> Option<&CompileError> {
        self.errors.first()
    }

    /// Iterate over all errors.
    pub fn iter(&self) -> impl Iterator<Item = &CompileError> {
        self.errors.iter()
    }

    /// Convert into an iterator over errors.
    pub fn into_iter(self) -> impl Iterator<Item = CompileError> {
        self.errors.into_iter()
    }

    /// Get all errors as a slice.
    pub fn as_slice(&self) -> &[CompileError] {
        &self.errors
    }

    /// Rebind every source location in every contained diagnostic.
    pub fn map_spans(self, mut map: impl FnMut(Span) -> Span) -> Self {
        Self {
            errors: self
                .errors
                .into_iter()
                .map(|error| error.map_spans(&mut map))
                .collect(),
        }
    }

    /// Check if the collection contains errors and return as a result.
    ///
    /// Returns `Ok(())` if empty, or `Err(self)` if there are errors.
    pub fn into_result(self) -> Result<(), CompileErrors> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }

    /// Fail with these errors if non-empty, otherwise return the value.
    ///
    /// This is useful for combining error checking with a result:
    /// ```ignore
    /// let output = SemaOutput { ... };
    /// errors.into_result_with(output)
    /// ```
    pub fn into_result_with<T>(self, value: T) -> Result<T, CompileErrors> {
        if self.is_empty() {
            Ok(value)
        } else {
            Err(self)
        }
    }
}

impl Default for CompileErrors {
    fn default() -> Self {
        Self::new()
    }
}

impl From<CompileError> for CompileErrors {
    fn from(error: CompileError) -> Self {
        Self::from_error(error)
    }
}

impl From<Vec<CompileError>> for CompileErrors {
    fn from(errors: Vec<CompileError>) -> Self {
        Self { errors }
    }
}

impl From<CompileErrors> for CompileError {
    /// Convert a collection to a single error.
    ///
    /// Uses the first error in the collection. If the collection is empty,
    /// returns an internal error (this indicates a compiler bug).
    fn from(errors: CompileErrors) -> Self {
        debug_assert!(
            !errors.is_empty(),
            "converting empty CompileErrors to CompileError"
        );
        errors.errors.into_iter().next().unwrap_or_else(|| {
            CompileError::without_span(ErrorKind::InternalError(
                "empty error collection converted to single error".into(),
            ))
        })
    }
}

impl fmt::Display for CompileErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.errors.len() {
            0 => write!(f, "no errors"),
            1 => write!(f, "{}", self.errors[0]),
            n => write!(
                f,
                "{} (and {} more error{})",
                self.errors[0],
                n - 1,
                if n == 2 { "" } else { "s" }
            ),
        }
    }
}

impl std::error::Error for CompileErrors {}

/// Result type for operations that can produce multiple errors.
pub type MultiErrorResult<T> = Result<T, CompileErrors>;

// ============================================================================
// Error Helper Traits
// ============================================================================

/// Extension trait for converting `Option<T>` to `CompileResult<T>`.
///
/// This trait simplifies the common pattern of converting lookup failures
/// (returning `None`) into compilation errors with source spans.
///
/// # Example
/// ```ignore
/// use rue_error::{OptionExt, ErrorKind};
///
/// let result = ctx.locals.get(name)
///     .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;
/// ```
pub trait OptionExt<T> {
    /// Convert `None` to a `CompileError` with the given kind and span.
    fn ok_or_compile_error(self, kind: ErrorKind, span: Span) -> CompileResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    #[inline]
    fn ok_or_compile_error(self, kind: ErrorKind, span: Span) -> CompileResult<T> {
        self.ok_or_else(|| CompileError::new(kind, span))
    }
}

/// The kind of compilation warning.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WarningKind {
    /// A variable was declared but never used.
    #[error("unused variable '{0}'")]
    UnusedVariable(String),
    /// A function was declared but never called.
    #[error("unused function '{0}'")]
    UnusedFunction(String),
    /// Code that will never be executed.
    #[error("unreachable code")]
    UnreachableCode,
    /// A pattern that will never be matched because a previous pattern already covers it.
    #[error("unreachable pattern '{0}'")]
    UnreachablePattern(String),
    /// A lookup's fallback sentinel is compared with the same sentinel to test absence.
    #[error("integer sentinel used to test lookup absence")]
    SentinelLookup,
}

/// A compilation warning with optional source location information.
///
/// Warnings don't stop compilation but indicate potential issues in the code.
///
/// Warnings can include rich diagnostic information using the builder methods:
/// ```ignore
/// CompileWarning::new(WarningKind::UnusedVariable("x".into()), span)
///     .with_help("if this is intentional, prefix it with an underscore: `_x`")
/// ```
pub type CompileWarning = DiagnosticWrapper<WarningKind>;

impl WarningKind {
    /// Returns the declaration name for warning kinds that should use line
    /// numbers to disambiguate duplicate names.
    pub fn unused_variable_name(&self) -> Option<&str> {
        match self {
            WarningKind::UnusedVariable(name) | WarningKind::UnusedFunction(name) => Some(name),
            _ => None,
        }
    }

    /// Format the warning message with an optional line number.
    ///
    /// When `line_number` is Some, the line number is appended to the message
    /// for warnings that have a name (like unused variables). This helps
    /// disambiguate when multiple variables share the same name.
    pub fn format_with_line(&self, line_number: Option<usize>) -> String {
        match (self, line_number) {
            (WarningKind::UnusedVariable(name), Some(line)) => {
                format!("unused variable '{}' (line {})", name, line)
            }
            (WarningKind::UnusedFunction(name), Some(line)) => {
                format!("unused function '{}' (line {})", name, line)
            }
            _ => self.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every ErrorCode constant must map to a distinct number: two diagnostics
    /// sharing an E-code would be indistinguishable to users and tests.
    /// Scans this file's source for the `pub const NAME: Self = Self(NNN);`
    /// declarations so the check can't go stale as codes are added.
    #[test]
    fn test_error_codes_are_unique() {
        let src = include_str!("lib.rs");
        let mut seen: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        for line in src.lines() {
            let Some(rest) = line.trim().strip_prefix("pub const ") else {
                continue;
            };
            let Some((name, tail)) = rest.split_once(": Self = Self(") else {
                continue;
            };
            let Some(num) = tail.split(')').next().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            if let Some(prev) = seen.insert(num, name.to_string()) {
                panic!("duplicate ErrorCode {num}: {prev} and {name}");
            }
        }
        assert!(
            seen.len() > 50,
            "expected to find the ErrorCode constants (found {})",
            seen.len()
        );
    }

    /// `ErrorKind::code()` must be injective: no two ErrorKind variants may
    /// map to the same ErrorCode constant, and every constant must be used by
    /// exactly one variant (a constant nobody maps to is dead, and usually a
    /// sign that a variant was repointed at someone else's code).
    ///
    /// `test_error_codes_are_unique` (above) guards constant *values*; this
    /// test guards the variant -> constant *mapping*. It scans the body of
    /// `fn code()` in this file's source, so it can't go stale as variants
    /// are added. It deliberately pins uniqueness, not specific code values.
    #[test]
    fn test_error_kind_to_code_mapping_is_injective() {
        let src = include_str!("lib.rs");

        // Collect all declared ErrorCode constant names.
        let mut declared: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for line in src.lines() {
            let Some(rest) = line.trim().strip_prefix("pub const ") else {
                continue;
            };
            if let Some((name, _)) = rest.split_once(": Self = Self(") {
                declared.insert(name);
            }
        }
        assert!(
            declared.len() > 50,
            "expected to find the ErrorCode constants (found {})",
            declared.len()
        );

        // Extract the body of `fn code()` by brace matching.
        let start = src
            .find("pub fn code(&self) -> ErrorCode {")
            .expect("fn code() not found in lib.rs");
        let open = start + src[start..].find('{').unwrap();
        let mut depth = 0usize;
        let mut end = open;
        for (i, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[open..=end];

        // Count every `ErrorCode::CONST` reference in the match arms.
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut rest = body;
        while let Some(pos) = rest.find("ErrorCode::") {
            rest = &rest[pos + "ErrorCode::".len()..];
            let name_len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let name = &rest[..name_len];
            *counts.entry(name).or_insert(0) += 1;
        }

        let duplicates: Vec<_> = counts.iter().filter(|&(_, &c)| c > 1).collect();
        assert!(
            duplicates.is_empty(),
            "ErrorCode constants mapped by more than one ErrorKind variant in code(): {:?}",
            duplicates
        );

        let referenced: std::collections::HashSet<&str> = counts.keys().copied().collect();
        let orphans: Vec<_> = declared.difference(&referenced).collect();
        assert!(
            orphans.is_empty(),
            "ErrorCode constants declared but never mapped by any ErrorKind variant: {:?}",
            orphans
        );
        let undeclared: Vec<_> = referenced.difference(&declared).collect();
        assert!(
            undeclared.is_empty(),
            "code() references ErrorCode constants that are not declared: {:?}",
            undeclared
        );
    }

    #[test]
    fn test_error_with_span() {
        let span = Span::new(10, 20);
        let error = CompileError::new(ErrorKind::InvalidInteger, span);

        assert!(error.has_span());
        assert_eq!(error.span(), Some(span));
        assert_eq!(error.to_string(), "invalid integer literal");
    }

    #[test]
    fn test_error_without_span() {
        let error = CompileError::without_span(ErrorKind::NoMainFunction);

        assert!(!error.has_span());
        assert_eq!(error.span(), None);
        assert_eq!(error.to_string(), "no main function found");
    }

    #[test]
    fn test_unexpected_character_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedCharacter('@'));
        assert_eq!(error.to_string(), "unexpected character: @");
    }

    #[test]
    fn test_unexpected_character_escapes_invisible_chars() {
        // Control and format characters must not appear raw in the one-line
        // message; escape_debug renders them as visible escapes.
        let vt = CompileError::without_span(ErrorKind::UnexpectedCharacter('\u{b}'));
        assert_eq!(vt.to_string(), "unexpected character: \\u{b}");
        let nul = CompileError::without_span(ErrorKind::UnexpectedCharacter('\0'));
        assert_eq!(nul.to_string(), "unexpected character: \\0");
        let bom = CompileError::without_span(ErrorKind::UnexpectedCharacter('\u{feff}'));
        assert_eq!(bom.to_string(), "unexpected character: \\u{feff}");
        // Printable characters are unchanged, including non-ASCII.
        let acute = CompileError::without_span(ErrorKind::UnexpectedCharacter('é'));
        assert_eq!(acute.to_string(), "unexpected character: é");
    }

    #[test]
    fn test_unexpected_token_message() {
        let error = CompileError::without_span(ErrorKind::UnexpectedToken {
            expected: Cow::Borrowed("identifier"),
            found: Cow::Borrowed("'+'"),
        });
        assert_eq!(error.to_string(), "expected identifier, found '+'");
    }

    #[test]
    fn test_parse_error_message() {
        let error =
            CompileError::without_span(ErrorKind::ParseError("custom parse error".to_string()));
        assert_eq!(error.to_string(), "custom parse error");
    }

    #[test]
    fn test_undefined_variable_message() {
        let error = CompileError::without_span(ErrorKind::UndefinedVariable("foo".to_string()));
        assert_eq!(error.to_string(), "undefined variable 'foo'");
    }

    #[test]
    fn test_undefined_function_message() {
        let error = CompileError::without_span(ErrorKind::UndefinedFunction("bar".to_string()));
        assert_eq!(error.to_string(), "undefined function 'bar'");
    }

    #[test]
    fn test_assign_to_immutable_message() {
        let error = CompileError::without_span(ErrorKind::AssignToImmutable("x".to_string()));
        assert_eq!(error.to_string(), "cannot assign to immutable variable 'x'");
    }

    #[test]
    fn test_unknown_type_message() {
        let error = CompileError::without_span(ErrorKind::UnknownType("Foo".to_string()));
        assert_eq!(error.to_string(), "unknown type 'Foo'");
    }

    #[test]
    fn test_type_mismatch_message() {
        let error = CompileError::without_span(ErrorKind::TypeMismatch {
            expected: "i32".to_string(),
            found: "bool".to_string(),
        });
        assert_eq!(error.to_string(), "type mismatch: expected i32, found bool");
    }

    #[test]
    fn test_wrong_argument_count_singular() {
        let error = CompileError::without_span(ErrorKind::WrongArgumentCount {
            expected: 1,
            found: 3,
        });
        assert_eq!(error.to_string(), "expected 1 argument, found 3");
    }

    #[test]
    fn test_wrong_argument_count_plural() {
        let error = CompileError::without_span(ErrorKind::WrongArgumentCount {
            expected: 2,
            found: 0,
        });
        assert_eq!(error.to_string(), "expected 2 arguments, found 0");
    }

    #[test]
    fn test_link_error_message() {
        let error =
            CompileError::without_span(ErrorKind::LinkError("undefined symbol".to_string()));
        assert_eq!(error.to_string(), "link error: undefined symbol");
    }

    #[test]
    fn test_error_kind_equality() {
        assert_eq!(ErrorKind::InvalidInteger, ErrorKind::InvalidInteger);
        assert_eq!(ErrorKind::NoMainFunction, ErrorKind::NoMainFunction);
        assert_ne!(ErrorKind::InvalidInteger, ErrorKind::NoMainFunction);
    }

    #[test]
    fn test_error_implements_std_error() {
        fn assert_error<T: std::error::Error>() {}
        assert_error::<CompileError>();
    }

    // ========================================================================
    // Diagnostic tests
    // ========================================================================

    #[test]
    fn test_diagnostic_empty_by_default() {
        let diag = Diagnostic::new();
        assert!(diag.is_empty());
        assert!(diag.labels.is_empty());
        assert!(diag.notes.is_empty());
        assert!(diag.helps.is_empty());
        assert!(diag.suggestions.is_empty());
    }

    #[test]
    fn test_diagnostic_not_empty_with_label() {
        let mut diag = Diagnostic::new();
        diag.labels.push(Label::new("test", Span::new(0, 10)));
        assert!(!diag.is_empty());
    }

    #[test]
    fn test_diagnostic_not_empty_with_note() {
        let mut diag = Diagnostic::new();
        diag.notes.push(Note::new("test note"));
        assert!(!diag.is_empty());
    }

    #[test]
    fn test_diagnostic_not_empty_with_help() {
        let mut diag = Diagnostic::new();
        diag.helps.push(Help::new("test help"));
        assert!(!diag.is_empty());
    }

    #[test]
    fn test_diagnostic_not_empty_with_suggestion() {
        let mut diag = Diagnostic::new();
        diag.suggestions
            .push(Suggestion::new("try this", Span::new(0, 10), "replacement"));
        assert!(!diag.is_empty());
    }

    #[test]
    fn test_label_creation() {
        let span = Span::new(10, 20);
        let label = Label::new("expected type here", span);
        assert_eq!(label.message, "expected type here");
        assert_eq!(label.span, span);
    }

    #[test]
    fn test_note_display() {
        let note = Note::new("types must match exactly");
        assert_eq!(note.to_string(), "types must match exactly");
    }

    #[test]
    fn test_help_display() {
        let help = Help::new("consider adding a type annotation");
        assert_eq!(help.to_string(), "consider adding a type annotation");
    }

    #[test]
    fn test_suggestion_creation() {
        let span = Span::new(10, 20);
        let suggestion = Suggestion::new("try this fix", span, "new_code");
        assert_eq!(suggestion.message, "try this fix");
        assert_eq!(suggestion.span, span);
        assert_eq!(suggestion.replacement, "new_code");
        assert_eq!(suggestion.applicability, Applicability::Unspecified);
    }

    #[test]
    fn test_suggestion_machine_applicable() {
        let span = Span::new(0, 5);
        let suggestion = Suggestion::machine_applicable("rename variable", span, "new_name");
        assert_eq!(suggestion.applicability, Applicability::MachineApplicable);
    }

    #[test]
    fn test_suggestion_maybe_incorrect() {
        let span = Span::new(0, 5);
        let suggestion = Suggestion::maybe_incorrect("try adding mut", span, "mut x");
        assert_eq!(suggestion.applicability, Applicability::MaybeIncorrect);
    }

    #[test]
    fn test_suggestion_with_placeholders() {
        let span = Span::new(0, 5);
        let suggestion = Suggestion::with_placeholders("add type annotation", span, ": <type>");
        assert_eq!(suggestion.applicability, Applicability::HasPlaceholders);
    }

    #[test]
    fn test_suggestion_with_applicability() {
        let span = Span::new(0, 5);
        let suggestion = Suggestion::new("fix", span, "new_code")
            .with_applicability(Applicability::MachineApplicable);
        assert_eq!(suggestion.applicability, Applicability::MachineApplicable);
    }

    #[test]
    fn test_applicability_display() {
        assert_eq!(
            Applicability::MachineApplicable.to_string(),
            "MachineApplicable"
        );
        assert_eq!(Applicability::MaybeIncorrect.to_string(), "MaybeIncorrect");
        assert_eq!(
            Applicability::HasPlaceholders.to_string(),
            "HasPlaceholders"
        );
        assert_eq!(Applicability::Unspecified.to_string(), "Unspecified");
    }

    #[test]
    fn test_applicability_default() {
        assert_eq!(Applicability::default(), Applicability::Unspecified);
    }

    #[test]
    fn test_error_with_suggestion() {
        let span = Span::new(10, 20);
        let error =
            CompileError::new(ErrorKind::AssignToImmutable("x".to_string()), span).with_suggestion(
                Suggestion::machine_applicable("add mut", Span::new(4, 5), "mut x"),
            );

        let diag = error.diagnostic();
        assert_eq!(diag.suggestions.len(), 1);
        assert_eq!(diag.suggestions[0].message, "add mut");
        assert_eq!(diag.suggestions[0].replacement, "mut x");
        assert_eq!(
            diag.suggestions[0].applicability,
            Applicability::MachineApplicable
        );
    }

    #[test]
    fn test_error_with_label() {
        let span = Span::new(10, 20);
        let label_span = Span::new(0, 5);
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            span,
        )
        .with_label("expected because of this", label_span);

        let diag = error.diagnostic();
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.labels[0].message, "expected because of this");
        assert_eq!(diag.labels[0].span, label_span);
    }

    #[test]
    fn test_error_with_note() {
        let span = Span::new(10, 20);
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            span,
        )
        .with_note("if and else branches must have compatible types");

        let diag = error.diagnostic();
        assert_eq!(diag.notes.len(), 1);
        assert_eq!(
            diag.notes[0].to_string(),
            "if and else branches must have compatible types"
        );
    }

    #[test]
    fn test_error_with_help() {
        let span = Span::new(10, 20);
        let error = CompileError::new(ErrorKind::AssignToImmutable("x".to_string()), span)
            .with_help("consider making `x` mutable: `let mut x`");

        let diag = error.diagnostic();
        assert_eq!(diag.helps.len(), 1);
        assert_eq!(
            diag.helps[0].to_string(),
            "consider making `x` mutable: `let mut x`"
        );
    }

    #[test]
    fn test_error_with_multiple_diagnostics() {
        let span = Span::new(10, 20);
        let label_span = Span::new(0, 5);
        let error = CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "i32".to_string(),
                found: "bool".to_string(),
            },
            span,
        )
        .with_label("then branch is here", label_span)
        .with_note("if and else branches must have compatible types")
        .with_help("consider using a type conversion");

        let diag = error.diagnostic();
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.notes.len(), 1);
        assert_eq!(diag.helps.len(), 1);
    }

    #[test]
    fn test_error_diagnostic_empty_by_default() {
        let span = Span::new(10, 20);
        let error = CompileError::new(ErrorKind::InvalidInteger, span);
        assert!(error.diagnostic().is_empty());
    }

    #[test]
    fn test_warning_with_help() {
        let span = Span::new(10, 20);
        let warning = CompileWarning::new(WarningKind::UnusedVariable("foo".to_string()), span)
            .with_help("if this is intentional, prefix it with an underscore: `_foo`");

        let diag = warning.diagnostic();
        assert_eq!(diag.helps.len(), 1);
        assert_eq!(
            diag.helps[0].to_string(),
            "if this is intentional, prefix it with an underscore: `_foo`"
        );
    }

    #[test]
    fn test_warning_with_label_and_note() {
        let span = Span::new(20, 25);
        let diverging_span = Span::new(10, 18);
        let warning = CompileWarning::new(WarningKind::UnreachableCode, span)
            .with_label(
                "any code following this expression is unreachable",
                diverging_span,
            )
            .with_note("this warning occurs because the preceding expression diverges");

        let diag = warning.diagnostic();
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.labels[0].span, diverging_span);
        assert_eq!(diag.notes.len(), 1);
    }

    #[test]
    fn test_warning_diagnostic_empty_by_default() {
        let span = Span::new(10, 20);
        let warning = CompileWarning::new(WarningKind::UnreachableCode, span);
        assert!(warning.diagnostic().is_empty());
    }

    // ========================================================================
    // Preview feature tests
    // ========================================================================

    #[test]
    fn test_preview_feature_test_infra() {
        let feature: PreviewFeature = "test_infra".parse().unwrap();
        assert_eq!(feature, PreviewFeature::TestInfra);
        assert_eq!(feature.name(), "test_infra");
        assert_eq!(feature.adr(), "ADR-0005");
    }

    #[test]
    fn test_preview_feature_from_str_unknown() {
        assert!("unknown".parse::<PreviewFeature>().is_err());
        assert!("".parse::<PreviewFeature>().is_err());
    }

    #[test]
    fn test_parse_preview_feature_error_display() {
        let err = "bad_feature".parse::<PreviewFeature>().unwrap_err();
        assert_eq!(err.to_string(), "unknown preview feature 'bad_feature'");
    }

    #[test]
    fn test_preview_feature_all_contains_test_infra() {
        let all = PreviewFeature::all();
        assert!(all.contains(&PreviewFeature::TestInfra));
    }

    #[test]
    fn test_preview_feature_all_names() {
        let names = PreviewFeature::all_names();
        assert_eq!(names, "test_infra, slices, c_ffi, borrow_accessors, floats");
    }

    #[test]
    fn test_preview_feature_floats() {
        // ADR-0065 (RUE-714): the floating-point preview feature, gating the
        // literal grammar in Phases 2-3 and everything M9 adds after it.
        let feature: PreviewFeature = "floats".parse().unwrap();
        assert_eq!(feature, PreviewFeature::Floats);
        assert_eq!(feature.name(), "floats");
        assert_eq!(feature.adr(), "ADR-0065");
        assert!(PreviewFeature::all().contains(&PreviewFeature::Floats));
    }

    #[test]
    fn test_float_literal_error_codes() {
        // The two halves of the float gate: E1100 without `--preview floats`
        // (the shared preview-gate kind), E1109 with it, plus the E0011
        // spelling rule from ADR-0065 §3.
        assert_eq!(
            ErrorKind::FloatNotYetImplemented.code(),
            ErrorCode::FLOAT_NOT_YET_IMPLEMENTED
        );
        assert_eq!(ErrorCode::FLOAT_NOT_YET_IMPLEMENTED.to_string(), "E1109");
        assert_eq!(
            ErrorKind::MalformedFloatLiteral("boom".to_string()).code(),
            ErrorCode::MALFORMED_FLOAT_LITERAL
        );
        assert_eq!(ErrorCode::MALFORMED_FLOAT_LITERAL.to_string(), "E0011");
        assert_eq!(
            ErrorKind::PreviewFeatureRequired {
                feature: PreviewFeature::Floats,
                what: "a floating-point literal".to_string(),
            }
            .code(),
            ErrorCode::PREVIEW_FEATURE_REQUIRED
        );
    }

    #[test]
    fn test_preview_feature_slices() {
        // ADR-0043 Phase 1 (RUE-322): the slice preview feature.
        let feature: PreviewFeature = "slices".parse().unwrap();
        assert_eq!(feature, PreviewFeature::Slices);
        assert_eq!(feature.name(), "slices");
        assert_eq!(feature.adr(), "ADR-0043");
        assert!(PreviewFeature::all().contains(&PreviewFeature::Slices));
    }

    #[test]
    fn test_slice_second_class_escape_codes() {
        // ADR-0043 Phase 1 (RUE-322): the second-class-escape diagnostics for
        // a slice type used outside argument position.
        assert_eq!(
            ErrorKind::SliceReturnNotAllowed.code(),
            ErrorCode::SLICE_RETURN_NOT_ALLOWED
        );
        assert_eq!(
            ErrorKind::SliceInAggregateField.code(),
            ErrorCode::SLICE_IN_AGGREGATE_FIELD
        );
        assert_eq!(
            ErrorKind::SliceEscapesScope.code(),
            ErrorCode::SLICE_ESCAPES_SCOPE
        );
        // Codes are contiguous with the not-yet-implemented gate (E0486).
        assert_eq!(ErrorCode::SLICE_RETURN_NOT_ALLOWED.0, 487);
        assert_eq!(ErrorCode::SLICE_IN_AGGREGATE_FIELD.0, 488);
        assert_eq!(ErrorCode::SLICE_ESCAPES_SCOPE.0, 489);
    }

    #[test]
    fn test_two_types_string_model_codes() {
        // ADR-0043 two-types model (RUE-386): first-class `str` vs. views.
        assert_eq!(
            ErrorKind::BufferNotFirstClassStr {
                found: "StrBuf".to_string(),
                site: "as a parameter argument".to_string(),
            }
            .code(),
            ErrorCode::BUFFER_NOT_FIRST_CLASS_STR
        );
        assert_eq!(
            ErrorKind::InoutStrRequiresLocalBuffer.code(),
            ErrorCode::INOUT_STR_REQUIRES_LOCAL_BUFFER
        );
        assert_eq!(
            ErrorKind::StrViewReassignment.code(),
            ErrorCode::STR_VIEW_REASSIGNMENT
        );
        assert_eq!(
            ErrorKind::StrViewNotFirstClass {
                site: "as a return value".to_string(),
            }
            .code(),
            ErrorCode::STR_VIEW_NOT_FIRST_CLASS
        );
        assert_eq!(ErrorCode::BUFFER_NOT_FIRST_CLASS_STR.0, 495);
        assert_eq!(ErrorCode::INOUT_STR_REQUIRES_LOCAL_BUFFER.0, 496);
        assert_eq!(ErrorCode::STR_VIEW_NOT_FIRST_CLASS.0, 497);
        assert_eq!(ErrorCode::STR_VIEW_REASSIGNMENT.0, 210);
    }

    #[test]
    fn test_preview_feature_test_infra_roundtrip() {
        use std::str::FromStr;
        let f = PreviewFeature::from_str("test_infra").unwrap();
        assert_eq!(f, PreviewFeature::TestInfra);
        assert_eq!(f.name(), "test_infra");
    }

    #[test]
    fn test_preview_feature_stabilized_are_unknown() {
        // for_loops, method_receivers, enum_payloads, array_repeat,
        // field_init_shorthand, inline_type_ctor_paths, raw_bytes, and
        // aggregate_layout were stabilized (no longer gated) — their names must
        // now be rejected.
        for name in [
            "for_loops",
            "method_receivers",
            "enum_payloads",
            "array_repeat",
            "field_init_shorthand",
            "inline_type_ctor_paths",
            "raw_bytes",
            "aggregate_layout",
        ] {
            assert!(
                name.parse::<PreviewFeature>().is_err(),
                "{name} should be an unknown preview feature after stabilization"
            );
        }
    }

    // ========================================================================
    // OptionExt trait tests
    // ========================================================================

    #[test]
    fn test_option_ext_some() {
        let span = Span::new(10, 20);
        let result: CompileResult<i32> =
            Some(42).ok_or_compile_error(ErrorKind::InvalidInteger, span);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_option_ext_none() {
        let span = Span::new(10, 20);
        let result: CompileResult<i32> = None.ok_or_compile_error(ErrorKind::InvalidInteger, span);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.span(), Some(span));
        assert!(matches!(error.kind, ErrorKind::InvalidInteger));
    }

    #[test]
    fn test_option_ext_with_complex_error() {
        let span = Span::new(5, 15);
        let result: CompileResult<String> =
            None.ok_or_compile_error(ErrorKind::UndefinedVariable("foo".to_string()), span);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.to_string(), "undefined variable 'foo'");
    }

    // ========================================================================
    // CompileErrors tests
    // ========================================================================

    #[test]
    fn test_compile_errors_new_is_empty() {
        let errors = CompileErrors::new();
        assert!(errors.is_empty());
        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn test_compile_errors_from_error() {
        let error = CompileError::without_span(ErrorKind::InvalidInteger);
        let errors = CompileErrors::from_error(error);
        assert!(!errors.is_empty());
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_compile_errors_push() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn test_compile_errors_extend() {
        let mut errors1 = CompileErrors::new();
        errors1.push(CompileError::without_span(ErrorKind::InvalidInteger));

        let mut errors2 = CompileErrors::new();
        errors2.push(CompileError::without_span(ErrorKind::NoMainFunction));
        errors2.push(CompileError::without_span(ErrorKind::BreakOutsideLoop));

        errors1.extend(errors2);
        assert_eq!(errors1.len(), 3);
    }

    #[test]
    fn test_compile_errors_first() {
        let mut errors = CompileErrors::new();
        assert!(errors.first().is_none());

        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));

        let first = errors.first().unwrap();
        assert!(matches!(first.kind, ErrorKind::InvalidInteger));
    }

    #[test]
    fn test_compile_errors_iter() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));

        let kinds: Vec<_> = errors.iter().map(|e| &e.kind).collect();
        assert_eq!(kinds.len(), 2);
    }

    #[test]
    fn test_compile_errors_into_result_empty() {
        let errors = CompileErrors::new();
        assert!(errors.into_result().is_ok());
    }

    #[test]
    fn test_compile_errors_into_result_non_empty() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        assert!(errors.into_result().is_err());
    }

    #[test]
    fn test_compile_errors_into_result_with() {
        let errors = CompileErrors::new();
        let result = errors.into_result_with(42);
        assert_eq!(result.unwrap(), 42);

        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        let result = errors.into_result_with(42);
        assert!(result.is_err());
    }

    #[test]
    fn test_compile_errors_from_single_error() {
        let error = CompileError::without_span(ErrorKind::InvalidInteger);
        let errors: CompileErrors = error.into();
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_compile_errors_to_single_error() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));

        let error: CompileError = errors.into();
        // Should get the first error
        assert!(matches!(error.kind, ErrorKind::InvalidInteger));
    }

    /// Test that empty CompileErrors conversion doesn't panic in release builds.
    /// In debug builds, this triggers a debug_assert panic (as expected).
    /// This test verifies the graceful fallback behavior in release mode.
    #[test]
    #[cfg_attr(debug_assertions, ignore)]
    fn test_empty_compile_errors_to_single_error() {
        // Converting an empty CompileErrors should not panic in release;
        // instead it should return an InternalError.
        let empty = CompileErrors::new();
        let error: CompileError = empty.into();

        // Should get an InternalError with a descriptive message
        match &error.kind {
            ErrorKind::InternalError(msg) => {
                assert!(msg.contains("empty error collection"));
            }
            other => panic!("expected InternalError, got {:?}", other),
        }
    }

    #[test]
    fn test_compile_errors_display_empty() {
        let errors = CompileErrors::new();
        assert_eq!(errors.to_string(), "no errors");
    }

    #[test]
    fn test_compile_errors_display_single() {
        let errors =
            CompileErrors::from_error(CompileError::without_span(ErrorKind::InvalidInteger));
        assert_eq!(errors.to_string(), "invalid integer literal");
    }

    #[test]
    fn test_compile_errors_display_multiple() {
        let mut errors = CompileErrors::new();
        errors.push(CompileError::without_span(ErrorKind::InvalidInteger));
        errors.push(CompileError::without_span(ErrorKind::NoMainFunction));
        assert_eq!(
            errors.to_string(),
            "invalid integer literal (and 1 more error)"
        );

        errors.push(CompileError::without_span(ErrorKind::BreakOutsideLoop));
        assert_eq!(
            errors.to_string(),
            "invalid integer literal (and 2 more errors)"
        );
    }

    // ========================================================================
    // Error code tests
    // ========================================================================

    #[test]
    fn test_error_code_display() {
        assert_eq!(ErrorCode::TYPE_MISMATCH.to_string(), "E0206");
        assert_eq!(ErrorCode::UNDEFINED_VARIABLE.to_string(), "E0201");
        assert_eq!(ErrorCode::INTERNAL_ERROR.to_string(), "E9000");
        assert_eq!(ErrorCode(1).to_string(), "E0001");
        assert_eq!(ErrorCode(42).to_string(), "E0042");
        assert_eq!(ErrorCode(1234).to_string(), "E1234");
    }

    #[test]
    fn test_error_kind_code_lexer() {
        assert_eq!(
            ErrorKind::UnexpectedCharacter('@').code(),
            ErrorCode::UNEXPECTED_CHARACTER
        );
        assert_eq!(ErrorKind::InvalidInteger.code(), ErrorCode::INVALID_INTEGER);
        assert_eq!(
            ErrorKind::InvalidStringEscape('n').code(),
            ErrorCode::INVALID_STRING_ESCAPE
        );
        assert_eq!(
            ErrorKind::UnterminatedString.code(),
            ErrorCode::UNTERMINATED_STRING
        );
        assert_eq!(
            ErrorKind::LexerDiagnosticsOmitted { limit: 100 }.code(),
            ErrorCode::LEXER_DIAGNOSTICS_OMITTED
        );
    }

    #[test]
    fn test_error_kind_code_parser() {
        assert_eq!(
            ErrorKind::UnexpectedToken {
                expected: "identifier".into(),
                found: "+".into()
            }
            .code(),
            ErrorCode::UNEXPECTED_TOKEN
        );
        assert_eq!(
            ErrorKind::ParseError("custom error".into()).code(),
            ErrorCode::PARSE_ERROR
        );
        assert_eq!(
            ErrorKind::ParserDiagnosticsOmitted { limit: 100 }.code(),
            ErrorCode::PARSER_DIAGNOSTICS_OMITTED
        );
    }

    #[test]
    fn test_error_kind_code_semantic() {
        assert_eq!(
            ErrorKind::NoMainFunction.code(),
            ErrorCode::NO_MAIN_FUNCTION
        );
        assert_eq!(
            ErrorKind::UndefinedVariable("x".into()).code(),
            ErrorCode::UNDEFINED_VARIABLE
        );
        assert_eq!(
            ErrorKind::UndefinedFunction("foo".into()).code(),
            ErrorCode::UNDEFINED_FUNCTION
        );
        assert_eq!(
            ErrorKind::TypeMismatch {
                expected: "i32".into(),
                found: "bool".into()
            }
            .code(),
            ErrorCode::TYPE_MISMATCH
        );
    }

    #[test]
    fn test_error_kind_code_control_flow() {
        assert_eq!(
            ErrorKind::BreakOutsideLoop.code(),
            ErrorCode::BREAK_OUTSIDE_LOOP
        );
        assert_eq!(
            ErrorKind::ContinueOutsideLoop.code(),
            ErrorCode::CONTINUE_OUTSIDE_LOOP
        );
        assert_eq!(
            ErrorKind::BreakWithValue.code(),
            ErrorCode::BREAK_WITH_VALUE
        );
    }

    #[test]
    fn test_error_kind_code_internal() {
        assert_eq!(
            ErrorKind::InternalError("bug".into()).code(),
            ErrorCode::INTERNAL_ERROR
        );
        assert_eq!(
            ErrorKind::InternalCodegenError("codegen bug".into()).code(),
            ErrorCode::INTERNAL_CODEGEN_ERROR
        );
    }

    #[test]
    fn test_error_code_equality() {
        assert_eq!(ErrorCode::TYPE_MISMATCH, ErrorCode(206));
        assert_ne!(ErrorCode::TYPE_MISMATCH, ErrorCode::UNDEFINED_VARIABLE);
    }

    #[test]
    fn test_error_code_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ErrorCode::TYPE_MISMATCH);
        set.insert(ErrorCode::UNDEFINED_VARIABLE);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&ErrorCode::TYPE_MISMATCH));
    }

    #[test]
    fn test_invalid_compiler_input_error_code_and_message() {
        let kind = ErrorKind::InvalidCompilerInput("duplicate file ID 7".into());
        assert_eq!(kind.code(), ErrorCode::INVALID_COMPILER_INPUT);
        assert_eq!(ErrorCode::INVALID_COMPILER_INPUT.0, 1400);
        assert_eq!(
            kind.to_string(),
            "invalid compiler input: duplicate file ID 7"
        );
    }

    #[test]
    fn compiler_resource_failures_have_distinct_non_ice_codes() {
        let limit = ErrorKind::CompilerResourceLimit("AIR words".into());
        let exhaustion = ErrorKind::CompilerResourceExhaustion("AIR reserve".into());
        let invariant = ErrorKind::CompilerProducerInvariant("bad AIR".into());
        assert_eq!(limit.code(), ErrorCode::COMPILER_RESOURCE_LIMIT);
        assert_eq!(exhaustion.code(), ErrorCode::COMPILER_RESOURCE_EXHAUSTION);
        assert_eq!(invariant.code(), ErrorCode::INTERNAL_ERROR);
        assert_ne!(limit.code(), exhaustion.code());
        assert!(!limit.to_string().contains("internal compiler"));
        assert!(!exhaustion.to_string().contains("internal compiler"));
        assert!(invariant.to_string().contains("internal compiler"));
    }

    // ========================================================================
    // ErrorKind size and boxing policy tests
    // ========================================================================

    #[test]
    fn test_error_kind_size() {
        // Measure the size of ErrorKind to understand current memory usage
        let size = std::mem::size_of::<ErrorKind>();

        // Enforce the size limit to prevent regression.
        // Target: ≤ 64 bytes (enum tag + largest inline variant)
        //
        // Current: 56 bytes (as of 2026-01-11)
        // - Enum discriminant: 8 bytes
        // - Largest inline variant: 48 bytes (2 Strings or 2 Cows)
        //
        // If this fails, check which variants are > 48 bytes and box them.
        assert!(
            size <= 64,
            "ErrorKind is {} bytes, exceeds 64-byte limit. \
             Consider boxing large variants (≥ 72 bytes / 3+ Strings). \
             See the boxing policy documentation above ErrorKind.",
            size
        );
    }

    #[test]
    fn test_error_kind_variant_sizes() {
        use std::mem::size_of;

        // Measure individual variant data sizes to identify which ones should be boxed
        println!("String: {} bytes", size_of::<String>());
        println!("Vec<String>: {} bytes", size_of::<Vec<String>>());
        println!(
            "Cow<'static, str>: {} bytes",
            size_of::<Cow<'static, str>>()
        );

        // Inline variants (currently unboxed)
        println!("TypeMismatch data: {} bytes", size_of::<(String, String)>());
        println!("UnknownField data: {} bytes", size_of::<(String, String)>());
        println!(
            "DuplicateField data: {} bytes",
            size_of::<(String, String)>()
        );
        println!(
            "ModuleNotFound data: {} bytes",
            size_of::<(String, Vec<String>)>()
        );

        // Boxed variants (currently boxed)
        println!(
            "MissingFieldsError: {} bytes",
            size_of::<MissingFieldsError>()
        );
        println!(
            "CopyStructNonCopyFieldError: {} bytes",
            size_of::<CopyStructNonCopyFieldError>()
        );
        println!(
            "IntrinsicTypeMismatchError: {} bytes",
            size_of::<IntrinsicTypeMismatchError>()
        );
    }
}
