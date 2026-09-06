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
//!   ([`crate::foreign_call::ForeignCallInputs`]) and the export entry
//!   ([`crate::export_thunk::ExportSignature`]) build;
//! * the native Rue convention reads [`rue_air::lower_native_signature`] for
//!   its arguments and [`rue_air::lower_native_return`] — through
//!   [`return_plan`] and [`return_registers`] — for its result: the same
//!   placements the callee's parameter storage plan, its return path, and every
//!   caller of that signature read (ADR-0084).
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
    RegisterPieces, ScalarAbiExtension, SourceParamAbi, Type, ValidatedAir,
};
use rue_cfg::{CfgInstData, CfgValue, ValidatedCfg};
use rue_target::{Arch, CRegisterClass, CallingConvention, SretRegisterKind, Target};

use crate::abi_slot_class::AbiSlotClass;
use crate::call_plan::{ReturnPlan, ReturnSlotReg, return_plan, return_registers};
use crate::native_abi::native_by_value_arg;

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
    /// In result registers: one piece per eightbyte, in ascending memory
    /// order. A result can span both banks, so the pieces are named
    /// individually rather than as one run.
    Result { pieces: RegisterPieces },
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
            LoweredReturn::Registers { pieces, .. } => Self::Result { pieces },
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

/// Where one value of the native Rue convention travels.
///
/// The native convention places every argument exactly where the compilation
/// target's C row places it (ADR-0084), so an argument's placement *is* a
/// [`CPlacement`]. The result is classified by the same rules against a wider
/// result roster, which is what the two return arms below describe: the
/// registers an aggregate's eightbytes occupy, or the caller storage its
/// indirect result is written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativePlacement {
    /// No slot at all (a zero-sized result).
    None,
    /// One argument, placed by the target's own C rules.
    Argument(CPlacement),
    /// The result is written to caller storage whose address travels in the
    /// target row's own indirect-result register.
    Sret {
        register: SretRegisterKind,
        echoed: bool,
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
    /// A `pub extern "C" fn` export: the C entry — an alias of the native body
    /// or a thunk — and the native body itself.
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
    /// How an export's C symbol is reached — as a second name for the native
    /// body, or through a thunk and then why. Present only for an export.
    pub entry: Option<crate::export_thunk::CEntry>,
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
/// The parameter grouping is the CFG's own [`SourceParamAbi`] descriptors, and
/// the placements come from the one signature lowering the prologue and every
/// caller consume ([`rue_air::lower_native_signature`]), so the report prints
/// the placement that will actually be emitted rather than a second opinion
/// about it.
fn native_side(
    cfg: &ValidatedCfg,
    air: &ValidatedAir,
    symbol: Option<&str>,
    type_pool: &FrozenTypeInternPool,
    target: Target,
) -> AbiSide {
    let return_type = cfg.return_type();
    let pairing = rue_target::ConventionSpec::native(target);
    let plan = return_plan(type_pool, return_type, pairing);

    // Two recoveries, because a body records the two parameter kinds
    // differently: a by-value parameter through its drop entry or `Param`
    // instruction, a by-reference one only through the pointee type on the
    // places that read it.
    let value_types = rue_air::body_parameter_types(air);
    let pointee_types = rue_air::by_reference_parameter_pointee_types(air);

    let descriptors = parameter_descriptors(cfg);
    let modes: Vec<AbiParameterMode> = descriptors
        .iter()
        .map(|descriptor| parameter_mode(cfg, descriptor))
        .collect();
    let types: Vec<Option<Type>> = descriptors
        .iter()
        .zip(&modes)
        .map(|(descriptor, mode)| {
            descriptor.ty.or_else(|| {
                let recovered = if *mode == AbiParameterMode::ByValue {
                    &value_types
                } else {
                    &pointee_types
                };
                recovered.get(&descriptor.start_slot).copied()
            })
        })
        .collect();
    // One parameter, one placement: the native convention places every value as
    // a whole, so the lowered signature's arguments line up with the
    // descriptors one-for-one.
    let parameters_facts = descriptors
        .iter()
        .zip(&modes)
        .map(|(descriptor, mode)| {
            let convention = if *mode == AbiParameterMode::ByValue {
                rue_air::ArgConvention::ByValue
            } else {
                rue_air::ArgConvention::ByReference
            };
            let native = match (convention, descriptor.ty) {
                (rue_air::ArgConvention::ByValue, Some(ty)) => native_by_value_arg(type_pool, ty),
                _ => crate::call_plan::register_width_leaf(),
            };
            (native.facts(), convention)
        })
        .collect::<Vec<_>>();
    let signature =
        rue_air::lower_native_signature(pairing, &parameters_facts, native_incoming_return(plan));

    let parameters = modes
        .iter()
        .zip(&types)
        .zip(signature.arguments())
        .enumerate()
        .map(|(index, ((mode, ty), argument))| AbiParameter {
            index: index as u32,
            ty: type_text(type_pool, *ty),
            mode: *mode,
            placement: AbiPlacement::Native(NativePlacement::Argument(CPlacement::from(
                argument.location,
            ))),
            extension: ScalarAbiExtension::None,
        })
        .collect();

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
                pairing,
            )),
            extension: ScalarAbiExtension::None,
        },
        stack_bytes: Some(signature.stack_bytes()),
    }
}

/// The already-decided return the native argument placement is computed
/// against: only the hidden indirect-result pointer affects where arguments go.
fn native_incoming_return(plan: ReturnPlan) -> LoweredReturn {
    let ReturnPlan::Sret {
        register, echoed, ..
    } = plan
    else {
        return LoweredReturn::Void;
    };
    LoweredReturn::Sret {
        register,
        echoed,
        size: rue_air::SLOT_BYTES as u32,
        align: rue_air::SLOT_BYTES as u32,
    }
}

fn native_return_placement(
    plan: ReturnPlan,
    type_pool: &FrozenTypeInternPool,
    ty: Type,
    pairing: rue_target::ConventionSpec,
) -> NativePlacement {
    match plan {
        ReturnPlan::ZeroSized => NativePlacement::None,
        ReturnPlan::Scalar => NativePlacement::ReturnRegisters {
            slots: vec![(primary_return_class(type_pool, ty), 0)],
        },
        ReturnPlan::Registers { .. } => NativePlacement::ReturnRegisters {
            slots: return_registers(type_pool, ty, pairing)
                .regs
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
            register,
            echoed,
        } => NativePlacement::Sret {
            // The indirect-result pointer travels where the target's own C row
            // puts it (ADR-0084).
            register,
            echoed,
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
        entry: None,
        target,
    }
}

/// One `pub extern "C" fn` export: its C entry and the native body that entry
/// names or forwards to.
///
/// `signature` is the very [`crate::export_thunk::ExportSignature`] the entry
/// is decided and generated from, so the C side printed here is the C side
/// emitted, and the entry line is the decision actually taken.
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
        entry: Some(signature.c_entry(target)),
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
            entry: None,
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

/// The register the hidden indirect-result pointer travels in under `register`:
/// the first general-purpose argument register when the row makes the pointer a
/// hidden first argument, and the row's dedicated register otherwise.
fn sret_pointer_text(registers: TargetRegisters, register: SretRegisterKind) -> String {
    match register {
        SretRegisterKind::ArgumentRegister => registers.argument(CRegisterClass::Gp, 0).to_owned(),
        SretRegisterKind::DedicatedRegister => registers.dedicated_sret().to_owned(),
    }
}

/// The result registers a value comes back in, named one per eightbyte. A run
/// of one bank reads as a run; a result split across banks names each piece,
/// because there is no single roster to run over.
fn result_register_text(
    registers: TargetRegisters,
    pieces: &[(CRegisterClass, u32)],
    noun: &str,
) -> String {
    let noun = format!("{noun} register");
    match pieces {
        [] => "no value".to_owned(),
        [(class, index)] => register_run(registers, RegisterRole::Result, *class, *index, 1, &noun),
        many if many.iter().all(|(class, _)| *class == many[0].0)
            && many
                .iter()
                .enumerate()
                .all(|(offset, (_, index))| *index == many[0].1 + offset as u32) =>
        {
            register_run(
                registers,
                RegisterRole::Result,
                many[0].0,
                many[0].1,
                many.len() as u32,
                &noun,
            )
        }
        many => many
            .iter()
            .map(|(class, index)| registers.result(*class, *index).to_owned())
            .collect::<Vec<_>>()
            .join(", "),
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

/// `1 <noun>` / `N <noun>s`, with the one irregular plural the report needs:
/// `leaf` becomes `leaves`.
fn counted(count: u32, noun: &str) -> String {
    if count == 1 {
        return format!("1 {noun}");
    }
    let plural = match noun.strip_suffix('f') {
        Some(stem) => format!("{stem}ves"),
        None => format!("{noun}s"),
    };
    format!("{count} {plural}")
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
        CPlacement::Result { pieces } => result_register_text(
            registers,
            &pieces
                .as_slice()
                .iter()
                .map(|piece| (piece.class, piece.index))
                .collect::<Vec<_>>(),
            "result",
        ),
        CPlacement::Sret {
            register,
            echoed,
            size,
            align,
        } => {
            let pointer = sret_pointer_text(registers, register);
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
        NativePlacement::Argument(placement) => {
            (c_placement_text(registers, *placement), Vec::new())
        }
        NativePlacement::Sret {
            register,
            echoed,
            slot_count,
            storage_bytes,
        } => (
            format!(
                "sret: pointer in {} to {} ({} of caller storage){}",
                sret_pointer_text(registers, *register),
                slots(*slot_count),
                bytes(*storage_bytes),
                if *echoed {
                    format!(", echoed in {}", registers.result(CRegisterClass::Gp, 0))
                } else {
                    String::new()
                }
            ),
            Vec::new(),
        ),
        NativePlacement::ReturnRegisters {
            slots: return_slots,
        } => match return_slots.as_slice() {
            [] => ("no value".to_owned(), Vec::new()),
            [_] => (
                result_register_text(registers, return_slots, "return"),
                Vec::new(),
            ),
            many => (
                format!("{} eightbytes", many.len()),
                many.iter()
                    .enumerate()
                    .map(|(index, piece)| {
                        format!(
                            "eightbyte {index}: {}",
                            result_register_text(registers, std::slice::from_ref(piece), "return")
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
        // callers see, then the native body behind it.
        if self.kind == AbiFunctionKind::Export {
            // Whether a C caller enters the native body directly is the fact
            // the two sides below are read against (ADR-0084), so it is stated
            // before them.
            match &self.entry {
                Some(crate::export_thunk::CEntry::Alias) => {
                    writeln!(f, "  c entry: alias of the native body")?
                }
                Some(crate::export_thunk::CEntry::Thunk(reason)) => {
                    writeln!(f, "  c entry: thunk, because {reason}")?
                }
                None => {}
            }
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
    fn a_native_argument_reads_like_a_c_one() {
        // The native convention places an argument exactly where the target's C
        // row places it (ADR-0084), so its block prints the same placement text.
        let registers = TargetRegisters::new(Target::X86_64Linux);
        let (line, continuation) = native_placement_text(
            registers,
            &NativePlacement::Argument(CPlacement::Registers {
                class: CRegisterClass::Gp,
                first: 0,
                count: 2,
            }),
        );
        assert_eq!(line, "gp registers 0-1 (rdi, rsi)");
        assert!(continuation.is_empty());

        let (line, _) = native_placement_text(
            registers,
            &NativePlacement::Argument(CPlacement::Stack {
                offset: 8,
                size: 24,
                align: 8,
            }),
        );
        assert_eq!(line, "stack +8 (24 bytes, align 8)");
    }
}
