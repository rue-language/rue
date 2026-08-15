//! Shared classification of intrinsic argument grammars.
//!
//! Most intrinsics take value expressions, but a few take types. The parser
//! consults this policy to parse those argument positions with the canonical
//! type grammar (the same one annotations use), and AstGen consults the same
//! list to lower the result as a `TypeIntrinsic`/`OffsetOf`. One shared list
//! keeps the two phases from disagreeing about which positions are types
//! (RUE-788).

/// Intrinsics whose every argument position takes a type. Each of these takes
/// exactly one argument; arity itself is diagnosed during semantic analysis,
/// so surplus arguments still parse as types rather than falling back to
/// expression parsing.
///
/// `require_droppable` is the compile-time well-formedness gate an owning
/// growable container (`std/arraybuf.rue`'s `ArrayBuf(T)`) uses to reject an
/// element type it cannot yet correctly own — one with a destructor or a
/// `linear` element (RUE-388). It takes the element type as its sole argument
/// and evaluates to unit during comptime type-constructor reduction, so it
/// must be lowered as a `TypeIntrinsic` (like `@size_of`), not an expression
/// call. `require_trivially_droppable` is the analogous by-copy-read gate
/// (RUE-651).
///
/// `int_max`/`int_min` are the integer-bounds intrinsics (RUE-694): they take
/// an integer type and evaluate to that type's largest/smallest value, typed
/// as the queried type itself.
pub const TYPE_INTRINSICS: &[&str] = &[
    "size_of",
    "align_of",
    "require_droppable",
    "require_trivially_droppable",
    "int_max",
    "int_min",
];

/// The field-offset intrinsic `@offset_of(T, field)` (RUE-301): its first
/// argument is a type and its second names a field of that type.
pub const OFFSET_OF_INTRINSIC: &str = "offset_of";

/// How many leading arguments of `@name(...)` are parsed with the type
/// grammar. Zero means the intrinsic is an ordinary value-position intrinsic
/// whose arguments take the full expression grammar.
pub fn type_argument_count(name: &str) -> usize {
    if TYPE_INTRINSICS.contains(&name) {
        usize::MAX
    } else if name == OFFSET_OF_INTRINSIC {
        1
    } else {
        0
    }
}
