//! Rue Intermediate Representation (RIR), the compiler's untyped IR.
//!
//! RIR is the first IR in the Rue compiler pipeline. The compiler lowers each
//! indexed declaration candidate through one [`AstGen`] authority. Semantic
//! analysis consumes that compact candidate artifact directly; whole-program
//! RIR presentation transiently composes the same artifacts into one dense
//! instruction space.
//!
//! The canonical construction sequence is:
//!
//! 1. Select an exact declaration with the parser's private candidate locator.
//! 2. Create [`AstGen::with_symbol_normalizer`] for that candidate and call
//!    [`AstGen::append_candidate_with_root`].
//! 3. Finish the candidate artifact. Consumers either analyze it directly or
//!    compose it using the parser's ordered declaration recipes.
//!
//! RIR remains untyped until semantic analysis, while its instructions and
//! variable-length payloads are stored densely and referenced by index. CLI
//! presentation remaps candidate-local references into canonical module and
//! program order without running a peer AST lowering path.
//!
//! Inspired by Zig's ZIR (Zig Intermediate Representation).

mod anonymous_sites;
#[cfg(test)]
mod api_inventory;
mod astgen;
mod inst;
mod type_syntax;

pub use anonymous_sites::{AnonymousTypeSite, AnonymousTypeSiteKind, anonymous_type_sites};
pub use astgen::{
    AstGen, AstGenCandidate, AstGenDeclarationRoot, AstGenFinishError, AstGenItemRoots,
};
pub use inst::{
    Inst, InstData, InstRef, InternalIntrinsic, MAX_RIR_ENTRIES_PER_PROGRAM, PackedRirAppend,
    PackedRirAppendError, PackedRirAppendMetadata, PackedRirDecodeError, PackedRirEncodeError,
    PackedRirMetadata, PackedRirMethodOwner, PackedRirProjection, PackedRirSymbols,
    PackedValidatedRir, RIR_PAYLOAD_FAMILY_NAMES, RIR_SPAN_FIELD_FAMILY_NAMES, RepeatCount, Rir,
    RirAnonEnumPayloadsRange, RirAnonEnumVariantsRange, RirAnonStructFieldsRange,
    RirAnonStructMethodsRange, RirArgMode, RirArrayElemsRange, RirBlockInstsRange, RirCallArg,
    RirCallArgsRange, RirDirective, RirDirectiveView, RirDirectivesRange, RirEditor,
    RirEnumPayloads, RirEnumPayloadsRange, RirEnumVariantsRange, RirFallibleIntrinsic,
    RirFallibleIntrinsicSet, RirFieldInitsRange, RirInternalIntrinsicArgsRange,
    RirIntrinsicArgsRange, RirMatchArmsRange, RirParam, RirParamMode, RirParamsRange, RirPattern,
    RirPatternView, RirPayloadBuildError, RirPayloadError, RirPayloadStorageStats, RirPrinter,
    RirSpanField, RirSpanRemapError, RirSpanSlot, RirSpanTraversalError, RirStructFieldsRange,
    RirStructMethodsRange, RirStructuralAnchor, RirStructuralPathSegment, RirValidationContext,
    ValidatedRir,
};
pub use type_syntax::{
    RirTypeSyntaxArena, RirTypeSyntaxBuildError, RirTypeSyntaxBuilder, RirTypeSyntaxNode,
    RirTypeSyntaxRange, RirTypeSyntaxRef, RirTypeSyntaxSymbol,
};
