//! Shared stack-verifier shell.
//!
//! Backends supply target-specific instruction effects and error formatting;
//! this module owns the common traversal, stack-depth tracking, and
//! control-flow join bookkeeping.

use ahash::AHashMap;
use rue_error::{CompileError, CompileResult};

use crate::vreg::LabelId;

/// Target-specific facts required by the shared stack verifier.
pub trait StackVerifyAdapter {
    /// Backend MIR instruction type.
    type Inst;

    /// Instruction sequence to verify.
    fn instructions(&self) -> &[Self::Inst];

    /// Stack-depth delta caused by `inst`, in bytes. Positive values mean the
    /// function body has moved the stack farther from the prologue-aligned
    /// state; negative values mean stack space was released.
    fn stack_delta(&self, inst: &Self::Inst) -> i64;

    /// Label defined by `inst`, if any.
    fn label(&self, inst: &Self::Inst) -> Option<LabelId>;

    /// Jump target labels referenced by `inst`.
    fn jump_targets<'a>(&self, inst: &'a Self::Inst) -> &'a [LabelId];

    /// Whether `inst` returns from the current function.
    fn is_return(&self, inst: &Self::Inst) -> bool;

    /// Return an alignment error message if `inst` observes an invalid stack
    /// depth for this target.
    fn alignment_violation(&self, inst: &Self::Inst, current_depth: i64) -> Option<String>;

    /// Build the backend's structured compile error.
    fn error(
        &self,
        idx: usize,
        inst: &Self::Inst,
        current_depth: i64,
        message: String,
    ) -> CompileError;
}

/// Verify stack alignment and control-flow join stack-depth consistency.
pub fn verify<A>(adapter: &A) -> CompileResult<()>
where
    A: StackVerifyAdapter,
{
    let mut verifier = StackVerifier::new(adapter);
    verifier.verify()
}

struct StackVerifier<'a, A: StackVerifyAdapter> {
    adapter: &'a A,
    /// Stack depth at each label (for join point verification).
    label_depths: AHashMap<LabelId, i64>,
    /// Current stack depth in bytes relative to the prologue-aligned stack.
    current_depth: i64,
}

impl<'a, A> StackVerifier<'a, A>
where
    A: StackVerifyAdapter,
{
    fn new(adapter: &'a A) -> Self {
        Self {
            adapter,
            label_depths: AHashMap::new(),
            current_depth: 0,
        }
    }

    fn verify(&mut self) -> CompileResult<()> {
        for (idx, inst) in self.adapter.instructions().iter().enumerate() {
            self.verify_instruction(idx, inst)?;
        }
        Ok(())
    }

    fn verify_instruction(&mut self, idx: usize, inst: &A::Inst) -> CompileResult<()> {
        self.current_depth += self.adapter.stack_delta(inst);

        if let Some(message) = self.adapter.alignment_violation(inst, self.current_depth) {
            return Err(self.adapter.error(idx, inst, self.current_depth, message));
        }

        if self.adapter.is_return(inst) && self.current_depth != 0 {
            return Err(self.adapter.error(
                idx,
                inst,
                self.current_depth,
                format!(
                    "return requires balanced stack depth, but depth is {} bytes",
                    self.current_depth
                ),
            ));
        }

        if let Some(label) = self.adapter.label(inst) {
            self.verify_label(idx, inst, label)?;
        }

        for &target in self.adapter.jump_targets(inst) {
            self.record_jump_target(target);
        }

        Ok(())
    }

    fn verify_label(&mut self, idx: usize, inst: &A::Inst, label: LabelId) -> CompileResult<()> {
        if let Some(&expected_depth) = self.label_depths.get(&label) {
            if expected_depth != self.current_depth {
                return Err(self.adapter.error(
                    idx,
                    inst,
                    self.current_depth,
                    format!(
                        "control flow join has inconsistent stack depth: expected {}, got {}",
                        expected_depth, self.current_depth
                    ),
                ));
            }
        } else {
            self.label_depths.insert(label, self.current_depth);
        }
        Ok(())
    }

    fn record_jump_target(&mut self, label: LabelId) {
        // If this is a forward jump, record the expected depth now and check it
        // when the label is encountered. If the label was already seen, the
        // entry preserves that observed depth, matching the previous verifier
        // behavior.
        self.label_depths.entry(label).or_insert(self.current_depth);
    }
}
