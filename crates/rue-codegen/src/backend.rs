//! The compiler-checked contract between the shared backend driver and a
//! target's leaf implementation.
//!
//! The contract deliberately describes orchestration capabilities, not a
//! common machine IR. `Mir` and `Reg` stay associated with each target, while
//! the existing liveness, allocation, slot, place-lowering, scheduling, and
//! stack-verification adapters remain the authorities that implement the
//! methods below.

use lasso::ThreadedRodeo;
use rue_air::FrozenTypeInternPool;
use rue_cfg::ValidatedCfg;
use rue_error::CompileResult;
use rue_target::Target;

use crate::codegen_pipeline::PreparedMir;
use crate::frame_layout::SavedRegScheme;
use crate::{BackendArtifactRequest, BackendArtifacts, MachineCode, MachineSymbolResolver};

/// The target backend contract consumed by the one generic code-generation
/// driver.
///
/// Associated types make it impossible to accidentally route one backend's
/// MIR, registers, or emitter through the other backend. Constants make ABI
/// facts used by the shared frame budget explicit. The remaining methods are
/// thin adapters to the target's existing pass authorities; they do not form
/// a second lowering or optimization path.
pub(crate) trait Backend {
    type Mir;
    type Reg;

    /// Architecture implemented by this backend. The shared driver checks it
    /// before validating or lowering a CFG, including in release builds.
    const ARCH: rue_target::Arch;
    const ARG_REG_COUNT: u32;
    const RETURN_REG_COUNT: u32;
    const SAVED_REG_SCHEME: SavedRegScheme;
    const TARGET_C_FLAVOR: rue_air::TargetCAbiFlavor;

    fn lower(
        cfg: &ValidatedCfg,
        type_pool: &FrozenTypeInternPool,
        interner: &ThreadedRodeo,
        target: Target,
        symbols: MachineSymbolResolver<'_>,
        request: BackendArtifactRequest,
        param_storage: &crate::param_storage::ParamStoragePlan,
        local_storage: &crate::local_storage::LocalSlotPlan,
        cancellation: crate::GenerationCancellation<'_>,
    ) -> CompileResult<(Self::Mir, BackendArtifacts)>;

    fn allocate(
        mir: Self::Mir,
        existing_slots: u32,
        artifacts: &mut BackendArtifacts,
        request: BackendArtifactRequest,
        cancellation: crate::GenerationCancellation<'_>,
    ) -> CompileResult<(Self::Mir, u32, Vec<Self::Reg>)>;

    fn peephole(mir: &mut Self::Mir);
    fn schedule(mir: &mut Self::Mir);
    fn verify(mir: &Self::Mir) -> CompileResult<()>;
    fn is_leaf(mir: &Self::Mir) -> bool;

    /// Return all global string IDs named by the target MIR.
    fn referenced_string_ids(mir: &Self::Mir) -> Vec<u32>;

    /// Rewrite the target MIR's string operands after local-table compaction.
    fn remap_string_ids(mir: &mut Self::Mir, remap: &std::collections::BTreeMap<u32, u32>);

    /// Emit the already-prepared MIR. This is the only target-specific part of
    /// the generic driver's final step: emitter construction and encoding stay
    /// in the target module and preserve its ABI and byte-level behavior.
    fn emit(
        prepared: &PreparedMir<Self::Mir, Self::Reg>,
        local_strings: &[String],
        request: BackendArtifactRequest,
        artifacts: &mut BackendArtifacts,
        cancellation: crate::GenerationCancellation<'_>,
    ) -> CompileResult<MachineCode>;
}
