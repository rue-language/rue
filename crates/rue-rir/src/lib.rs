//! Rue Intermediate Representation (RIR), the compiler's untyped IR.
//!
//! RIR is the first IR in the Rue compiler pipeline. The compiler lowers the
//! canonical parsed modules for one source revision in a single [`AstGen`]
//! session, producing one dense instruction space for the complete program.
//! Module provenance is tracked by the compiler alongside that RIR; it is not
//! represented by separate per-file RIR fragments or a post-hoc merge step.
//!
//! The canonical construction sequence is:
//!
//! 1. Create [`AstGen::with_symbol_normalizer`] with the revision's semantic
//!    interner and symbol translation function.
//! 2. Call [`AstGen::append_items`] once for each module in canonical order.
//! 3. Call [`AstGen::finish`] to obtain the program-wide RIR.
//!
//! RIR remains untyped until semantic analysis, while its instructions and
//! variable-length payloads are stored densely and referenced by index. CLI
//! presentation may apply a read-only module-order permutation without
//! changing the canonical instruction references.
//!
//! Inspired by Zig's ZIR (Zig Intermediate Representation).

mod anonymous_sites;
#[cfg(test)]
mod api_inventory;
mod astgen;
mod inst;

pub use anonymous_sites::{AnonymousTypeSite, AnonymousTypeSiteKind, anonymous_type_sites};
pub use astgen::AstGen;
pub use inst::{
    Inst, InstData, InstRef, InternalIntrinsic, RIR_PAYLOAD_FAMILY_NAMES, RepeatCount, Rir,
    RirAnonEnumPayloadsRange, RirAnonEnumVariantsRange, RirAnonStructFieldsRange,
    RirAnonStructMethodsRange, RirArgMode, RirArrayElemsRange, RirBlockInstsRange, RirCallArg,
    RirCallArgsRange, RirDirective, RirDirectiveView, RirDirectivesRange, RirEditor,
    RirEnumPayloads, RirEnumPayloadsRange, RirEnumVariantsRange, RirFieldInitsRange,
    RirInternalIntrinsicArgsRange, RirIntrinsicArgsRange, RirMatchArmsRange, RirParam,
    RirParamMode, RirParamsRange, RirPattern, RirPatternView, RirPayloadBuildError,
    RirPayloadError, RirPayloadStorageStats, RirPrinter, RirStructFieldsRange,
    RirStructMethodsRange, RirStructuralAnchor, RirStructuralPathSegment, RirValidationContext,
    ValidatedRir,
};
