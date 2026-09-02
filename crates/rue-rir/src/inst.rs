//! RIR instruction definitions.
//!
//! Instructions are stored in a dense array and referenced by index.
//! This provides good cache locality and efficient traversal.

use std::fmt;
use std::marker::PhantomData;

use lasso::{Key, Spur};
use rue_span::{FileId, Span};

use crate::type_syntax::{RirTypeSyntaxArena, RirTypeSyntaxBuilder, RirTypeSyntaxRef};

mod payload;
mod printer;

pub use payload::*;
pub use printer::*;

/// The complete canonical RIR for one source revision.
#[derive(Debug, Default)]
pub struct Rir {
    /// All instructions across the canonical module sequence.
    instructions: Vec<Inst>,
    /// Producer-private structural paths for source variable reads. Entries
    /// are aligned with `instructions`; public flat anchors remain in
    /// `InstData::VarRef` only for explicitly constructed RIR.
    deferred_structural_anchors: Vec<Option<RirDeferredStructuralAnchor>>,
    /// Extra data for variable-length instruction payloads.
    extra: Vec<u32>,
    /// Declaration-local structured type syntax referenced by type-bearing
    /// instruction and payload slots. Leaf spellings use the same candidate
    /// symbol universe as the instruction graph; compound syntax is never
    /// rendered into that universe.
    type_syntax: RirTypeSyntaxArena<Spur>,
    /// Set once `add_inst` is asked for an instruction beyond the published
    /// `u32` instruction ceiling (spec Appendix C.6:1). `add_inst` is called
    /// from hundreds of infallible lowering sites, so the ceiling is recorded
    /// here and reported once at the construction/publication boundary
    /// (`AstGen::try_finish_editor`, `RirEditor::capacity_error`) instead of
    /// wrapping an `InstRef` onto the reserved null payload. Spec C.1:2
    /// requires a diagnostic, not a wrapped index.
    instruction_limit_exceeded: bool,
    /// Set only after the complete owner passes publication validation. This
    /// lets the same accessor API retain fail-closed construction for raw RIR
    /// while giving published owners lazy fixed-record views.
    views_validated: bool,
}

impl PartialEq for Rir {
    fn eq(&self, other: &Self) -> bool {
        self.instructions == other.instructions
            && self.deferred_structural_anchors == other.deferred_structural_anchors
            && self.extra == other.extra
            && self.type_syntax == other.type_syntax
            && self.instruction_limit_exceeded == other.instruction_limit_exceeded
    }
}

impl Eq for Rir {}
