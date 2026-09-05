//! The `--emit abi` report: where every value of every reachable function's
//! signature actually travels (RUE-2033).
//!
//! ## One source of truth
//!
//! This module classifies nothing. It projects the artifacts code generation
//! already consumes onto text:
//!
//! * a C boundary — a reached `extern "C"` import call and the C entry of a
//!   `pub extern "C" fn` export — reads [`rue_air::lower_c_signature`]'s
//!   [`LoweredSignature`], the one placement function every C crossing site
//!   consumes, through the very same projections the import lowering
//!   ([`crate::foreign_call::ForeignCallInputs`]) and the export thunk
//!   ([`crate::export_thunk::ExportSignature`]) build;
//! * the native Rue convention reads [`rue_air::NativeCallAbi`] through
//!   [`return_plan`] for the classification and [`assign_abi_slots`] /
//!   [`crate::call_plan::return_slot_regs`] for the physical slot plan, over the
//!   same class vector the callee's parameter storage plan builds.
//!
//! Physical register *names* stay in the two backends: this module asks
//! [`TargetRegisters`] for the name of a roster index and never restates a
//! roster, so a backend that renames or reorders a roster changes the report
//! with it.
//!
//! ## What the report cannot show
//!
//! An `extern` or export signature naming a type that fails an FFI predicate
//! ([`rue_air::FfiPredicateFailure`]) is rejected while its *signature* is
//! resolved — before any body is analyzed, and therefore before this report has
//! a function to describe. Such a program produces no ABI report at all:
//! `--emit abi` takes the driver's ordinary error path and prints the
//! diagnostic, which the `emit_abi_ffi_predicate_failure_takes_the_error_path`
//! CLI case pins.
//!
//! ## Structure
//!
//! [`AbiFunctionReport`] and the placement types are data; [`std::fmt::Display`]
//! is one projection of that data. A machine-readable projection would be
//! another projection of the same values, not a second walk of the artifacts.

use lasso::ThreadedRodeo;
use rue_air::{
    ArgLocation, FrozenTypeInternPool, LoweredReturn, LoweredSignature, PointerLocation,
    ScalarAbiExtension, SourceParamAbi, Type, ValidatedAir, native_return_register_budget,
};
use rue_cfg::{CfgInstData, CfgValue, ValidatedCfg};
use rue_target::{Arch, CRegisterClass, CallingConvention, SretRegisterKind, Target};

use crate::call_plan::{
    AbiRegisterBanks, AbiSlotClass, AbiSlotLocation, ReturnPlan, ReturnSlotReg, assign_abi_slots,
    return_plan, return_slot_regs,
};

// ============================================================================
// Register naming
// ============================================================================

/// The physical register rosters of one target, as the backend that owns them
/// names them.
///
/// Both rosters are the *native* Rue convention's, and both C rows reuse them:
/// a platform argument register is this argument roster's entry (`rdi..r9`,
/// `x0..x7`) and a platform result register is this return roster's (`rax, rdx`
/// on SysV; `x0, x1` on AAPCS64). How many of each a convention may use comes
/// from [`rue_target::CConventionSpec`], never from these lengths, so this type
/// answers only "what is register `n` of bank `c` called".
#[derive(Debug, Clone, Copy)]
pub struct TargetRegisters {
    arch: Arch,
}

#[derive(Debug, Clone, Copy)]
enum RegisterRole {
    Argument,
    Result,
}

impl TargetRegisters {
    /// The rosters of `target`'s architecture.
    pub fn new(target: Target) -> Self {
        Self {
            arch: target.arch(),
        }
    }

    fn roster(self, role: RegisterRole, class: CRegisterClass) -> &'static [&'static str] {
        match (self.arch, role, class) {
            (Arch::X86_64, RegisterRole::Argument, CRegisterClass::Gp) => {
                &crate::x86_64::GP_ARGUMENT_REGISTER_NAMES
            }
            (Arch::X86_64, RegisterRole::Argument, CRegisterClass::Fp) => {
                &crate::x86_64::FP_ARGUMENT_REGISTER_NAMES
            }
            (Arch::X86_64, RegisterRole::Result, CRegisterClass::Gp) => {
                &crate::x86_64::GP_RETURN_REGISTER_NAMES
            }
            (Arch::X86_64, RegisterRole::Result, CRegisterClass::Fp) => {
                &crate::x86_64::FP_RETURN_REGISTER_NAMES
            }
            (Arch::Aarch64, RegisterRole::Argument, CRegisterClass::Gp) => {
                &crate::aarch64::GP_ARGUMENT_REGISTER_NAMES
            }
            (Arch::Aarch64, RegisterRole::Argument, CRegisterClass::Fp) => {
                &crate::aarch64::FP_ARGUMENT_REGISTER_NAMES
            }
            (Arch::Aarch64, RegisterRole::Result, CRegisterClass::Gp) => {
                &crate::aarch64::GP_RETURN_REGISTER_NAMES
            }
            (Arch::Aarch64, RegisterRole::Result, CRegisterClass::Fp) => {
                &crate::aarch64::FP_RETURN_REGISTER_NAMES
            }
        }
    }

    /// The name of argument register `index` of `class`. `?` when the index is
    /// past the roster, which no placement produces.
    pub fn argument(self, class: CRegisterClass, index: u32) -> &'static str {
        self.roster(RegisterRole::Argument, class)
            .get(index as usize)
            .copied()
            .unwrap_or("?")
    }

    /// The name of result register `index` of `class`.
    pub fn result(self, class: CRegisterClass, index: u32) -> &'static str {
        self.roster(RegisterRole::Result, class)
            .get(index as usize)
            .copied()
            .unwrap_or("?")
    }

    /// The register carrying the hidden indirect-result pointer under a
    /// convention that dedicates one outside the argument roster (AAPCS64's
    /// `x8`, section 6.9).
    pub fn dedicated_sret(self) -> &'static str {
        match self.arch {
            Arch::X86_64 => crate::x86_64::DEDICATED_SRET_REGISTER_NAME,
            Arch::Aarch64 => crate::aarch64::DEDICATED_SRET_REGISTER_NAME,
        }
    }

    fn argument_banks(self) -> AbiRegisterBanks {
        AbiRegisterBanks {
            gp: self
                .roster(RegisterRole::Argument, CRegisterClass::Gp)
                .len(),
            fp: self
                .roster(RegisterRole::Argument, CRegisterClass::Fp)
                .len(),
        }
    }

    fn return_banks(self) -> AbiRegisterBanks {
        AbiRegisterBanks {
            gp: self.roster(RegisterRole::Result, CRegisterClass::Gp).len(),
            fp: self.roster(RegisterRole::Result, CRegisterClass::Fp).len(),
        }
    }
}

// ============================================================================
// Report data
// ============================================================================

/// How a source parameter is presented before ABI classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiParameterMode {
    /// A normal by-value parameter.
    ByValue,
    /// A `borrow` parameter: one read-only caller pointer.
    Borrow,
    /// An `inout` parameter: one writable caller pointer.
    Inout,
}

impl AbiParameterMode {
    /// The source spelling of this mode.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ByValue => "by value",
            Self::Borrow => "borrow",
            Self::Inout => "inout",
        }
    }
}

/// Where one value of a C signature travels, as
/// [`rue_air::lower_c_signature`] placed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CPlacement {
    /// No register, no stack byte, no pointer.
    Omitted,
    /// `count` consecutive registers of `class` from roster index `first`.
    Registers {
        class: CRegisterClass,
        first: u32,
        count: u32,
    },
    /// By value in the outgoing argument area.
    Stack { offset: u32, size: u32, align: u32 },
    /// By pointer to a caller-owned copy.
    Indirect {
        pointer: PointerLocation,
        size: u32,
        align: u32,
    },
    /// In result registers.
    Result { class: CRegisterClass, count: u32 },
    /// Through caller storage whose address crosses as a hidden argument.
    Sret {
        register: SretRegisterKind,
        echoed: bool,
        size: u32,
        align: u32,
    },
    /// No value crosses back.
    Void,
}

impl From<ArgLocation> for CPlacement {
    fn from(location: ArgLocation) -> Self {
        match location {
            ArgLocation::Omitted => Self::Omitted,
            ArgLocation::Registers { pieces } => Self::Registers {
                class: pieces.uniform_class().expect(
                    "a C argument's registers are one bank while the boundary rejects floats",
                ),
                first: pieces.first_index().unwrap_or(0),
                count: pieces.len(),
            },
            ArgLocation::Stack {
                offset,
                size,
                align,
            } => Self::Stack {
                offset,
                size,
                align,
            },
            ArgLocation::Indirect {
                pointer,
                size,
                align,
            } => Self::Indirect {
                pointer,
                size,
                align,
            },
        }
    }
}

impl From<LoweredReturn> for CPlacement {
    fn from(ret: LoweredReturn) -> Self {
        match ret {
            LoweredReturn::Void => Self::Void,
            LoweredReturn::Registers { class, count, .. } => Self::Result { class, count },
            LoweredReturn::Sret {
                register,
                echoed,
                size,
                align,
            } => Self::Sret {
                register,
                echoed,
                size,
                align,
            },
        }
    }
}

/// One flattened native ABI slot's physical position, and which logical slot of
/// its value rides there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSlot {
    /// The value's own logical slot index. The native convention reverses a
    /// multi-slot by-value aggregate, so this descends across such a
    /// parameter's positions.
    pub logical: u32,
    /// The position the shared slot assignment gave it.
    pub location: AbiSlotLocation,
}

/// Where one value of the native Rue convention travels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativePlacement {
    /// No slot at all (a zero-sized result).
    None,
    /// The value's flattened slots, in ABI order.
    Slots {
        slots: Vec<NativeSlot>,
        /// Whether the slots cross in reverse logical order, which the native
        /// convention does for every multi-slot by-value aggregate.
        reversed: bool,
    },
    /// One pointer slot standing for caller storage: a `borrow` / `inout`
    /// parameter, or a by-value aggregate the compact layout forces indirect.
    Pointer {
        location: AbiSlotLocation,
        /// How many of the callee's parameter slots the pointed-at value
        /// occupies, for the by-value indirect case where the classifier
        /// collapsed a wider value onto one incoming register. `None` for a
        /// by-reference parameter, whose single slot holds only the pointer and
        /// says nothing about the pointee.
        pointee_slots: Option<u32>,
    },
    /// The result is written to caller storage whose address is the hidden
    /// first ABI slot.
    Sret {
        location: AbiSlotLocation,
        slot_count: u32,
        storage_bytes: u32,
    },
    /// The result comes back one logical slot per return register.
    ReturnRegisters {
        /// One `(bank, roster index)` per logical slot, in logical order.
        slots: Vec<(CRegisterClass, u32)>,
    },
}

/// A placement under whichever convention the enclosing side follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiPlacement {
    C(CPlacement),
    Native(NativePlacement),
}

/// One parameter of one side of one function's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiParameter {
    /// Source position, counting from zero.
    pub index: u32,
    /// Structural spelling of the parameter's type
    /// ([`rue_air::drop_glue_names::type_name`]), or `?` when the analyzed body
    /// recovered no type for it.
    pub ty: String,
    pub mode: AbiParameterMode,
    pub placement: AbiPlacement,
    /// The extension a narrow integer carries at a C boundary.
    pub extension: ScalarAbiExtension,
}

/// One signature's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiReturn {
    pub ty: String,
    pub placement: AbiPlacement,
    pub extension: ScalarAbiExtension,
}

/// One side of one function's ABI: a convention and the placements it fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiSide {
    pub convention: CallingConvention,
    /// The machine symbol this side is entered through, when it has one.
    pub symbol: Option<String>,
    pub parameters: Vec<AbiParameter>,
    pub ret: AbiReturn,
    /// Bytes the caller reserves for the outgoing argument area. Present for a
    /// C side, whose convention measures the area in bytes.
    pub stack_bytes: Option<u32>,
}

/// What kind of boundary a report block describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiFunctionKind {
    /// An ordinary Rue function, method, or reached generic instance.
    Function,
    /// A reached `extern "C"` import: a C side only, since Rue compiles no body
    /// for it.
    Import,
    /// A `pub extern "C" fn` export: the C entry thunk and the native body it
    /// forwards to.
    Export,
}

impl AbiFunctionKind {
    const fn keyword(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Import => "import",
            Self::Export => "export",
        }
    }
}

/// One function's block of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiFunctionReport {
    /// The name the block is headed by: the source name of a native function,
    /// the unmangled C symbol of an import or export.
    pub name: String,
    pub kind: AbiFunctionKind,
    /// The C side, present for an import and an export.
    pub c: Option<AbiSide>,
    /// The native side, present for a function and an export.
    pub native: Option<AbiSide>,
    /// The target whose rosters name the registers.
    pub target: Target,
}

// ============================================================================
// Projection: the native Rue convention
// ============================================================================

fn type_text(type_pool: &FrozenTypeInternPool, ty: Option<Type>) -> String {
    match ty {
        Some(ty) => rue_air::drop_glue_names::type_name(ty, type_pool),
        None => "?".to_owned(),
    }
}

/// The parameter grouping this report walks.
///
/// Real compilation always carries the classifier's own [`SourceParamAbi`]
/// descriptors. A directly constructed CFG (synthetic backend tests) carries
/// none, and code generation then homes one incoming register per parameter
/// slot. The report takes the same fallback under the same condition
/// `crate::param_storage::ParamStoragePlan::plan` does — no descriptors, or
/// descriptors that do not tile the parameter area exactly — so it describes
/// the prologue that will actually be emitted rather than metadata code
/// generation is about to ignore.
fn parameter_descriptors(cfg: &ValidatedCfg) -> Vec<SourceParamAbi> {
    let descriptors = cfg.source_param_abi();
    let tiles = !descriptors.is_empty()
        && descriptors.iter().map(|d| d.slot_count).sum::<u32>() == cfg.num_params()
        && descriptors.first().is_some_and(|d| d.start_slot == 0)
        && descriptors
            .iter()
            .zip(descriptors.iter().skip(1))
            .all(|(a, b)| a.start_slot + a.slot_count == b.start_slot);
    if tiles {
        return descriptors.to_vec();
    }
    (0..cfg.num_params())
        .map(|slot| SourceParamAbi {
            start_slot: slot,
            slot_count: 1,
            crossing_regs: 1,
            crossing_classes: vec![rue_air::NativeArgClass::Gp],
            ty: None,
        })
        .collect()
}

fn parameter_mode(cfg: &ValidatedCfg, descriptor: &SourceParamAbi) -> AbiParameterMode {
    let slot = descriptor.start_slot;
    if !cfg
        .param_modes()
        .get(slot as usize)
        .copied()
        .unwrap_or(false)
    {
        return AbiParameterMode::ByValue;
    }
    if cfg.is_param_writable(slot) {
        AbiParameterMode::Inout
    } else {
        AbiParameterMode::Borrow
    }
}

/// Project one function's native ABI from its CFG.
///
/// The parameter grouping is the CFG's own [`SourceParamAbi`] descriptors — the
/// classification semantic analysis recorded — and the physical positions come
/// from [`assign_abi_slots`] over the same class vector
/// `crate::param_storage::ParamStoragePlan` builds, so the report and the
/// prologue read one assignment rather than two.
fn native_side(
    cfg: &ValidatedCfg,
    air: &ValidatedAir,
    symbol: Option<&str>,
    type_pool: &FrozenTypeInternPool,
    target: Target,
) -> AbiSide {
    let registers = TargetRegisters::new(target);
    let return_type = cfg.return_type();
    let plan = return_plan(
        type_pool,
        return_type,
        native_return_register_budget(target.arch()),
    );

    let descriptors = parameter_descriptors(cfg);
    let mut classes = Vec::new();
    if plan.uses_sret() {
        classes.push(AbiSlotClass::Gp);
    }
    for descriptor in &descriptors {
        classes.extend(
            descriptor
                .crossing_classes
                .iter()
                .copied()
                .map(AbiSlotClass::from),
        );
    }
    let locations = assign_abi_slots(
        rue_target::ConventionSpec::native(target),
        classes.iter().copied(),
        registers.argument_banks(),
    );

    // Two recoveries, because a body records the two parameter kinds
    // differently: a by-value parameter through its drop entry or `Param`
    // instruction, a by-reference one only through the pointee type on the
    // places that read it.
    let value_types = rue_air::body_parameter_types(air);
    let pointee_types = rue_air::by_reference_parameter_pointee_types(air);
    let mut cursor = usize::from(plan.uses_sret());
    let mut parameters = Vec::with_capacity(descriptors.len());
    for (index, descriptor) in descriptors.iter().enumerate() {
        let crossing = descriptor.crossing_regs as usize;
        let positions = &locations[cursor..cursor + crossing];
        cursor += crossing;
        let mode = parameter_mode(cfg, descriptor);
        let ty = descriptor.ty.or_else(|| {
            let recovered = if mode == AbiParameterMode::ByValue {
                &value_types
            } else {
                &pointee_types
            };
            recovered.get(&descriptor.start_slot).copied()
        });
        parameters.push(AbiParameter {
            index: index as u32,
            ty: type_text(type_pool, ty),
            mode,
            placement: AbiPlacement::Native(native_parameter_placement(
                descriptor, mode, ty, type_pool, positions,
            )),
            extension: ScalarAbiExtension::None,
        });
    }

    AbiSide {
        convention: CallingConvention::Rue,
        symbol: symbol.map(str::to_owned),
        parameters,
        ret: AbiReturn {
            ty: type_text(type_pool, Some(return_type)),
            placement: AbiPlacement::Native(native_return_placement(
                plan,
                type_pool,
                return_type,
                registers,
                locations.first().copied(),
            )),
            extension: ScalarAbiExtension::None,
        },
        stack_bytes: None,
    }
}

fn native_parameter_placement(
    descriptor: &SourceParamAbi,
    mode: AbiParameterMode,
    ty: Option<Type>,
    type_pool: &FrozenTypeInternPool,
    positions: &[AbiSlotLocation],
) -> NativePlacement {
    // One incoming pointer standing for a wider value: a by-reference
    // parameter, or a by-value aggregate the compact layout forced indirect
    // (`crossing_regs < slot_count`).
    if mode != AbiParameterMode::ByValue || descriptor.is_by_value_indirect() {
        return match positions.first() {
            Some(&location) => NativePlacement::Pointer {
                location,
                pointee_slots: descriptor
                    .is_by_value_indirect()
                    .then_some(descriptor.slot_count),
            },
            None => NativePlacement::None,
        };
    }
    // The native convention reverses a multi-slot by-value aggregate's slots so
    // the callee reconstructs the ascending frame layout (`CallPlan` reverses
    // the materialized slots before assigning them), so ABI position `k` of
    // such a parameter carries logical slot `count - 1 - k`.
    let reversed = positions.len() > 1
        && ty.is_some_and(|ty| crate::types::is_multislot_aggregate(type_pool, ty));
    let slots = positions
        .iter()
        .enumerate()
        .map(|(position, &location)| NativeSlot {
            logical: if reversed {
                positions.len() as u32 - 1 - position as u32
            } else {
                position as u32
            },
            location,
        })
        .collect();
    NativePlacement::Slots { slots, reversed }
}

fn native_return_placement(
    plan: ReturnPlan,
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    registers: TargetRegisters,
    hidden_slot: Option<AbiSlotLocation>,
) -> NativePlacement {
    match plan {
        ReturnPlan::ZeroSized => NativePlacement::None,
        ReturnPlan::Scalar => NativePlacement::ReturnRegisters {
            slots: vec![(primary_return_class(type_pool, ty), 0)],
        },
        ReturnPlan::Registers { .. } => NativePlacement::ReturnRegisters {
            slots: return_slot_regs(type_pool, ty, registers.return_banks())
                .into_iter()
                .map(|register| match register {
                    ReturnSlotReg::Gp(index) => (CRegisterClass::Gp, index as u32),
                    ReturnSlotReg::Fp { index, .. } => (CRegisterClass::Fp, index as u32),
                })
                .collect(),
        },
        ReturnPlan::Sret {
            slot_count,
            storage_bytes,
        } => NativePlacement::Sret {
            // The hidden pointer is ABI slot 0 by construction, so the first
            // assigned location is its own.
            location: hidden_slot.unwrap_or(AbiSlotLocation::GpReg(0)),
            slot_count,
            storage_bytes,
        },
    }
}

/// The bank the primary return register of a one-slot result belongs to: a
/// float leaf comes back in the floating-point file, everything else in the
/// general-purpose one — the same leaf rule [`AbiSlotClass::for_leaf`] applies
/// to arguments.
fn primary_return_class(type_pool: &FrozenTypeInternPool, ty: Type) -> CRegisterClass {
    match crate::types::aggregate_leaf_types(type_pool, ty)
        .first()
        .copied()
        .map(AbiSlotClass::for_leaf)
    {
        Some(AbiSlotClass::Fp(_)) => CRegisterClass::Fp,
        _ => CRegisterClass::Gp,
    }
}

// ============================================================================
// Projection: a C boundary
// ============================================================================

/// Project one lowered C signature. `parameter_types` and `return_type` are the
/// source spellings the block prints beside each placement; the placements
/// themselves are `signature`'s alone.
fn c_side(
    signature: &LoweredSignature,
    symbol: Option<&str>,
    parameter_types: &[String],
    return_type: String,
) -> AbiSide {
    let parameters = signature
        .arguments()
        .iter()
        .enumerate()
        .map(|(index, argument)| AbiParameter {
            index: index as u32,
            ty: parameter_types
                .get(index)
                .cloned()
                .unwrap_or_else(|| "?".to_owned()),
            // Semantic analysis rejects `borrow` / `inout` in an `extern "C"`
            // signature, so every C parameter is by value.
            mode: AbiParameterMode::ByValue,
            placement: AbiPlacement::C(argument.location.into()),
            extension: argument.extension,
        })
        .collect();
    AbiSide {
        convention: signature.convention(),
        symbol: symbol.map(str::to_owned),
        parameters,
        ret: AbiReturn {
            ty: return_type,
            placement: AbiPlacement::C(signature.ret().into()),
            extension: match signature.ret() {
                LoweredReturn::Registers { extension, .. } => extension,
                _ => ScalarAbiExtension::None,
            },
        },
        stack_bytes: Some(signature.stack_bytes()),
    }
}

// ============================================================================
// Entry points
// ============================================================================

/// One reached Rue function, method, or generic instance.
pub fn function_abi_report(
    cfg: &ValidatedCfg,
    air: &ValidatedAir,
    source_name: &str,
    symbol: &str,
    type_pool: &FrozenTypeInternPool,
    target: Target,
) -> AbiFunctionReport {
    AbiFunctionReport {
        name: source_name.to_owned(),
        kind: AbiFunctionKind::Function,
        c: None,
        native: Some(native_side(cfg, air, Some(symbol), type_pool, target)),
        target,
    }
}

/// One `pub extern "C" fn` export: the C entry the thunk implements, and the
/// native body it forwards to.
///
/// `signature` is the very [`crate::export_thunk::ExportSignature`] the thunk
/// generator consumes, so the C side printed here is the C side emitted.
pub fn export_abi_report(
    exported_symbol: &str,
    native_symbol: &str,
    signature: &crate::export_thunk::ExportSignature,
    cfg: &ValidatedCfg,
    air: &ValidatedAir,
    type_pool: &FrozenTypeInternPool,
    target: Target,
) -> AbiFunctionReport {
    let native = native_side(cfg, air, Some(native_symbol), type_pool, target);
    let parameter_types = native
        .parameters
        .iter()
        .map(|parameter| parameter.ty.clone())
        .collect::<Vec<_>>();
    let return_type = native.ret.ty.clone();
    AbiFunctionReport {
        name: exported_symbol.to_owned(),
        kind: AbiFunctionKind::Export,
        c: Some(c_side(
            &signature.lowered(),
            Some(exported_symbol),
            &parameter_types,
            return_type,
        )),
        native: Some(native),
        target,
    }
}

/// Every `extern "C"` import one function reaches, in call order.
///
/// The signature comes from [`crate::foreign_call::ForeignCallInputs::from_cfg`]
/// against the call site's own argument and result types — the same
/// construction the import lowering performs — so an import's report is the
/// placement its call sequence writes.
pub fn import_abi_reports(
    cfg: &ValidatedCfg,
    interner: &ThreadedRodeo,
    symbols: &crate::MachineSymbolResolver<'_>,
    type_pool: &FrozenTypeInternPool,
    target: Target,
) -> Vec<AbiFunctionReport> {
    let mut reports = Vec::new();
    for raw in 0..cfg.value_count() {
        let value = CfgValue::from_raw(raw as u32);
        let inst = cfg.get_inst(value);
        let CfgInstData::Call { name, runtime, .. } = &inst.data else {
            continue;
        };
        if runtime.is_some() {
            continue;
        }
        let symbol = symbols.resolve(interner.resolve(name));
        // The convention is the import declaration's own, resolved from its ABI
        // string once in semantic analysis (spec 9.3:1b), so the report prints
        // the row the call sequence is actually written under.
        let Some(convention) = symbols.foreign_convention(&symbol) else {
            continue;
        };
        let args = cfg.get_call_args(&inst.data);
        let inputs = crate::foreign_call::ForeignCallInputs::from_cfg(
            symbol.clone(),
            cfg,
            type_pool,
            inst.ty,
            args,
            convention,
        );
        let parameter_types = args
            .iter()
            .map(|arg| type_text(type_pool, Some(cfg.get_inst(arg.value).ty)))
            .collect::<Vec<_>>();
        reports.push(AbiFunctionReport {
            name: symbol.clone(),
            kind: AbiFunctionKind::Import,
            c: Some(c_side(
                inputs.signature(),
                Some(&symbol),
                &parameter_types,
                type_text(type_pool, Some(inst.ty)),
            )),
            native: None,
            target,
        });
    }
    reports
}

// ============================================================================
// Text
// ============================================================================

fn extension_text(extension: ScalarAbiExtension) -> String {
    match extension {
        ScalarAbiExtension::None => String::new(),
        ScalarAbiExtension::Signed { from_bits } => {
            format!(", sign-extended from {from_bits} bits")
        }
        ScalarAbiExtension::Unsigned { from_bits } => {
            format!(", zero-extended from {from_bits} bits")
        }
    }
}

const fn bank(class: CRegisterClass) -> &'static str {
    match class {
        CRegisterClass::Gp => "gp",
        CRegisterClass::Fp => "fp",
    }
}

fn register_run(
    registers: TargetRegisters,
    role: RegisterRole,
    class: CRegisterClass,
    first: u32,
    count: u32,
    noun: &str,
) -> String {
    let name = |index: u32| match role {
        RegisterRole::Argument => registers.argument(class, index),
        RegisterRole::Result => registers.result(class, index),
    };
    if count <= 1 {
        return format!("{} {noun} {first} ({})", bank(class), name(first));
    }
    let names = (first..first + count)
        .map(name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} {noun}s {first}-{} ({names})",
        bank(class),
        first + count - 1
    )
}

fn native_slot_text(registers: TargetRegisters, location: AbiSlotLocation) -> String {
    match location {
        AbiSlotLocation::GpReg(index) => register_run(
            registers,
            RegisterRole::Argument,
            CRegisterClass::Gp,
            index as u32,
            1,
            "register",
        ),
        AbiSlotLocation::FpReg(index) => register_run(
            registers,
            RegisterRole::Argument,
            CRegisterClass::Fp,
            index as u32,
            1,
            "register",
        ),
        AbiSlotLocation::Stack {
            offset,
            size,
            align,
        } => format!("stack +{offset} ({}, align {align})", bytes(size)),
    }
}

/// Where an indirectly-passed argument's own pointer lives, with the
/// preposition its position wants: a pointer travels *in* a register and sits
/// *at* an offset in the outgoing argument area.
fn c_pointer_text(registers: TargetRegisters, pointer: PointerLocation) -> String {
    match pointer {
        PointerLocation::Register { index } => format!(
            "in {}",
            register_run(
                registers,
                RegisterRole::Argument,
                CRegisterClass::Gp,
                index,
                1,
                "register",
            )
        ),
        PointerLocation::Stack { offset } => format!("at stack +{offset}"),
    }
}

/// `1 byte` / `N bytes`, so a one-byte Darwin stack slot does not read as
/// `1 bytes`.
fn bytes(count: u32) -> String {
    counted(count, "byte")
}

/// `1 slot` / `N slots`.
fn slots(count: u32) -> String {
    counted(count, "slot")
}

fn counted(count: u32, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The one-line rendering of a placement, plus any continuation lines it needs.
fn placement_text(registers: TargetRegisters, placement: &AbiPlacement) -> (String, Vec<String>) {
    match placement {
        AbiPlacement::C(placement) => (c_placement_text(registers, *placement), Vec::new()),
        AbiPlacement::Native(placement) => native_placement_text(registers, placement),
    }
}

fn c_placement_text(registers: TargetRegisters, placement: CPlacement) -> String {
    match placement {
        CPlacement::Omitted => "omitted (zero-sized)".to_owned(),
        CPlacement::Registers {
            class,
            first,
            count,
        } => register_run(
            registers,
            RegisterRole::Argument,
            class,
            first,
            count,
            "register",
        ),
        CPlacement::Stack {
            offset,
            size,
            align,
        } => format!("stack +{offset} ({}, align {align})", bytes(size)),
        CPlacement::Indirect {
            pointer,
            size,
            align,
        } => format!(
            "indirect: pointer {} to {} (align {align})",
            c_pointer_text(registers, pointer),
            bytes(size)
        ),
        CPlacement::Result { class, count } => register_run(
            registers,
            RegisterRole::Result,
            class,
            0,
            count,
            "result register",
        ),
        CPlacement::Sret {
            register,
            echoed,
            size,
            align,
        } => {
            let pointer = match register {
                SretRegisterKind::ArgumentRegister => {
                    registers.argument(CRegisterClass::Gp, 0).to_owned()
                }
                SretRegisterKind::DedicatedRegister => registers.dedicated_sret().to_owned(),
            };
            let echo = if echoed {
                format!(", echoed in {}", registers.result(CRegisterClass::Gp, 0))
            } else {
                String::new()
            };
            format!(
                "sret: pointer in {pointer}{echo} ({}, align {align})",
                bytes(size)
            )
        }
        CPlacement::Void => "no value".to_owned(),
    }
}

fn native_placement_text(
    registers: TargetRegisters,
    placement: &NativePlacement,
) -> (String, Vec<String>) {
    match placement {
        NativePlacement::None => ("no value".to_owned(), Vec::new()),
        NativePlacement::Slots {
            slots: value_slots,
            reversed,
        } => match value_slots.as_slice() {
            [] => ("omitted (no ABI slot)".to_owned(), Vec::new()),
            [only] => (native_slot_text(registers, only.location), Vec::new()),
            many => (
                // Whether a multi-slot value crosses reversed is the native
                // convention's least guessable rule, so it is stated either
                // way rather than implied by its absence.
                format!(
                    "{}, {}",
                    slots(many.len() as u32),
                    if *reversed {
                        "reversed"
                    } else {
                        "in logical order"
                    }
                ),
                many.iter()
                    .map(|slot| {
                        format!(
                            "slot {}: {}",
                            slot.logical,
                            native_slot_text(registers, slot.location)
                        )
                    })
                    .collect(),
            ),
        },
        NativePlacement::Pointer {
            location,
            pointee_slots,
        } => (
            match pointee_slots {
                Some(count) => format!(
                    "indirect: pointer in {} to {}",
                    native_slot_text(registers, *location),
                    slots(*count)
                ),
                None => format!(
                    "indirect: pointer in {}",
                    native_slot_text(registers, *location)
                ),
            },
            Vec::new(),
        ),
        NativePlacement::Sret {
            location,
            slot_count,
            storage_bytes,
        } => (
            format!(
                "sret: pointer in {} to {} ({} of caller storage)",
                native_slot_text(registers, *location),
                slots(*slot_count),
                bytes(*storage_bytes)
            ),
            Vec::new(),
        ),
        NativePlacement::ReturnRegisters {
            slots: return_slots,
        } => match return_slots.as_slice() {
            [] => ("no value".to_owned(), Vec::new()),
            [(class, index)] => (
                register_run(
                    registers,
                    RegisterRole::Result,
                    *class,
                    *index,
                    1,
                    "return register",
                ),
                Vec::new(),
            ),
            many => (
                slots(many.len() as u32),
                many.iter()
                    .enumerate()
                    .map(|(logical, (class, index))| {
                        format!(
                            "slot {logical}: {}",
                            register_run(
                                registers,
                                RegisterRole::Result,
                                *class,
                                *index,
                                1,
                                "return register",
                            )
                        )
                    })
                    .collect(),
            ),
        },
    }
}

fn write_side(
    f: &mut std::fmt::Formatter<'_>,
    side: &AbiSide,
    registers: TargetRegisters,
    indent: &str,
) -> std::fmt::Result {
    match &side.symbol {
        Some(symbol) => writeln!(f, "{indent}convention {}, symbol {symbol}", side.convention)?,
        None => writeln!(f, "{indent}convention {}", side.convention)?,
    }
    for parameter in &side.parameters {
        let (line, continuation) = placement_text(registers, &parameter.placement);
        writeln!(
            f,
            "{indent}parameter {}: {}, {}, {line}{}",
            parameter.index,
            parameter.ty,
            parameter.mode.name(),
            extension_text(parameter.extension)
        )?;
        for extra in continuation {
            writeln!(f, "{indent}  {extra}")?;
        }
    }
    let (line, continuation) = placement_text(registers, &side.ret.placement);
    writeln!(
        f,
        "{indent}return: {}, {line}{}",
        side.ret.ty,
        extension_text(side.ret.extension)
    )?;
    for extra in continuation {
        writeln!(f, "{indent}  {extra}")?;
    }
    if let Some(bytes) = side.stack_bytes {
        writeln!(f, "{indent}outgoing argument area: {bytes} bytes")?;
    }
    Ok(())
}

impl std::fmt::Display for AbiFunctionReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let registers = TargetRegisters::new(self.target);
        writeln!(f, "{} {}", self.kind.keyword(), self.name)?;
        // An export prints both halves of the crossing it owns: the C entry
        // callers see, then the native body the thunk forwards to.
        if self.kind == AbiFunctionKind::Export {
            if let Some(side) = &self.c {
                writeln!(f, "  c side")?;
                write_side(f, side, registers, "    ")?;
            }
            if let Some(side) = &self.native {
                writeln!(f, "  native side")?;
                write_side(f, side, registers, "    ")?;
            }
            return Ok(());
        }
        if let Some(side) = self.c.as_ref().or(self.native.as_ref()) {
            write_side(f, side, registers, "  ")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_target::Target;

    /// The C rows name registers out of the backend rosters this module asks,
    /// so a C placement can only be rendered correctly while each row's budget
    /// fits inside the roster it indexes.
    #[test]
    fn every_c_row_indexes_inside_the_backend_rosters_it_names() {
        for target in Target::all() {
            let registers = TargetRegisters::new(*target);
            let spec = target.c_calling_convention().c_spec();
            for class in [CRegisterClass::Gp, CRegisterClass::Fp] {
                let arguments = registers.roster(RegisterRole::Argument, class);
                let results = registers.roster(RegisterRole::Result, class);
                assert!(
                    spec.argument_registers(class) as usize <= arguments.len(),
                    "{target:?} {class:?} argument roster is smaller than its psABI budget"
                );
                assert!(
                    spec.return_registers(class) as usize <= results.len(),
                    "{target:?} {class:?} result roster is smaller than its psABI budget"
                );
                assert!(arguments.iter().all(|name| !name.is_empty()));
                assert!(results.iter().all(|name| !name.is_empty()));
            }
        }
    }

    #[test]
    fn the_sysv_row_names_its_own_registers() {
        let registers = TargetRegisters::new(Target::X86_64Linux);
        assert_eq!(registers.argument(CRegisterClass::Gp, 0), "rdi");
        assert_eq!(registers.argument(CRegisterClass::Gp, 2), "rdx");
        assert_eq!(registers.result(CRegisterClass::Gp, 0), "rax");
        assert_eq!(registers.result(CRegisterClass::Gp, 1), "rdx");
        assert_eq!(
            c_placement_text(
                registers,
                CPlacement::Sret {
                    register: SretRegisterKind::ArgumentRegister,
                    echoed: true,
                    size: 24,
                    align: 8,
                }
            ),
            "sret: pointer in rdi, echoed in rax (24 bytes, align 8)"
        );
    }

    #[test]
    fn the_aapcs64_row_names_its_dedicated_indirect_result_register() {
        let registers = TargetRegisters::new(Target::Aarch64Macos);
        assert_eq!(registers.argument(CRegisterClass::Gp, 7), "x7");
        assert_eq!(registers.dedicated_sret(), "x8");
        assert_eq!(
            c_placement_text(
                registers,
                CPlacement::Sret {
                    register: SretRegisterKind::DedicatedRegister,
                    echoed: false,
                    size: 24,
                    align: 8,
                }
            ),
            "sret: pointer in x8 (24 bytes, align 8)"
        );
    }

    #[test]
    fn a_stacked_and_an_indirect_placement_state_their_physical_detail() {
        let registers = TargetRegisters::new(Target::X86_64Linux);
        assert_eq!(
            c_placement_text(
                registers,
                CPlacement::Stack {
                    offset: 16,
                    size: 8,
                    align: 8,
                }
            ),
            "stack +16 (8 bytes, align 8)"
        );
        assert_eq!(
            c_placement_text(
                registers,
                CPlacement::Indirect {
                    pointer: PointerLocation::Register { index: 2 },
                    size: 24,
                    align: 8,
                }
            ),
            "indirect: pointer in gp register 2 (rdx) to 24 bytes (align 8)"
        );
        assert_eq!(
            c_placement_text(registers, CPlacement::Omitted),
            "omitted (zero-sized)"
        );
    }

    #[test]
    fn a_reversed_multislot_parameter_names_the_logical_slot_each_register_carries() {
        let registers = TargetRegisters::new(Target::X86_64Linux);
        let (line, continuation) = native_placement_text(
            registers,
            &NativePlacement::Slots {
                slots: vec![
                    NativeSlot {
                        logical: 2,
                        location: AbiSlotLocation::GpReg(0),
                    },
                    NativeSlot {
                        logical: 1,
                        location: AbiSlotLocation::GpReg(1),
                    },
                    NativeSlot {
                        logical: 0,
                        location: AbiSlotLocation::stack_slot(0),
                    },
                ],
                reversed: true,
            },
        );
        assert_eq!(line, "3 slots, reversed");
        assert_eq!(
            continuation,
            [
                "slot 2: gp register 0 (rdi)",
                "slot 1: gp register 1 (rsi)",
                "slot 0: stack +0 (8 bytes, align 8)",
            ]
        );
    }
}
