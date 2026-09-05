//! The one calling-convention value type and the one `"C"` alias table.
//!
//! [`CallingConvention`] names a *concrete* convention: the compiler-chosen
//! native Rue convention, or one of the platform psABIs Rue targets. It is the
//! only spelling of "which calling convention governs this boundary" in the
//! tree — semantic analysis, the stable query plane, both code-generation
//! backends, and the runtime-helper boundary all name these rows.
//!
//! ## Why this crate is the home
//!
//! A convention row is a *target* fact, so the type belongs wherever every
//! consumer can already see a target. `rue-target` is that place: it is a leaf
//! with no dependencies, and `rue-air`, `rue-cfg`, `rue-codegen`, and
//! `rue-compiler` all depend on it already. Putting the type here also puts the
//! `"C"` alias table next to the [`Target`] rows it is keyed by, so a new
//! target cannot compile without answering which psABI it follows.
//!
//! The one crate that cannot see a [`Target`] is `rue-runtime-abi`, the
//! `no_std`, dependency-free compiler/runtime ABI manifest (ADR-0055). It is
//! not given a peer convention enum and it does not gain a dependency: its
//! manifest rows are target-independent, so they record only that a helper
//! crosses the platform C boundary, and the concrete row is resolved by the
//! caller — which always has a `Target` — through
//! [`CallingConvention::c_for_target`]. `RuntimeTarget` reaches the same table
//! through the `Target` correspondence in `rue-codegen`, so the alias has
//! exactly one mapping table.
//!
//! ## Conventions as data
//!
//! Every psABI rule this compiler needs from a C row is a field of
//! [`CConventionSpec`], and [`CallingConvention::c_spec`] is the one table that
//! answers it: the argument and result register roster *sizes*, where the
//! hidden indirect-result pointer travels and whether the callee echoes it, the
//! call-boundary stack alignment and shadow space, how the outgoing argument
//! area is packed, who extends a narrow integer, and which aggregate rule
//! applies. The physical register *names* stay in the two code-generation
//! backends, which map a roster index to a register; nothing else about a
//! convention is a `match` anywhere else in the tree.
//!
//! The description lives here rather than in `rue-air` because it needs no type
//! facts: every field is a property of the psABI alone. The classifier that
//! reads it against a type's size, alignment, and scalar kind is
//! `rue_air::lowered_signature`, which is where type facts first exist.
//!
//! ## Apple's arm64 amendments
//!
//! `Aarch64Aapcs` and `Aarch64AapcsDarwin` are separate rows because Apple's
//! platform ABI amends AAPCS64 ("Writing ARM64 code for Apple platforms"). The
//! amendments Rue's supported surface reaches are the caller-side extension of
//! arguments narrower than 32 bits and, the one that changes placement,
//! [`stacked_argument_packing`](CallingConvention::stacked_argument_packing):
//! a stacked argument occupies its natural size at its natural alignment
//! instead of a whole 8-byte slot. This type states the convention; how far the
//! foreign-call lowering implements it (scalars today, composites still on
//! whole eightbytes) is recorded in
//! `docs/notes/ffi-abi-conformance-audit.md`.

use crate::Target;

/// Which calling convention governs a call boundary.
///
/// The variants are concrete conventions, not families: there is no "target C"
/// row to be resolved later. `"C"` is an alias that a [`Target`] resolves to one
/// of these rows through [`CallingConvention::c_for_target`].
///
/// The ordering is declaration order and carries no meaning; it exists so a
/// convention can sit inside the ordered durable facts that carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CallingConvention {
    /// The native, unstable, compiler-chosen Rue convention (RUE-106): a
    /// by-value aggregate is returned one flattened ABI slot per return
    /// register, or via sret when it does not fit; canonical `StrBuf` always
    /// returns via sret; a by-reference `inout` / `borrow` argument is one
    /// pointer slot; a by-value argument occupies one slot per leaf, reversed
    /// within each multi-slot value.
    Rue,
    /// The System V AMD64 psABI, as used on x86-64 Linux.
    X86_64SysV,
    /// The ARM 64-bit Procedure Call Standard (AAPCS64), as used on AArch64
    /// Linux.
    Aarch64Aapcs,
    /// AAPCS64 with Apple's arm64 amendments, as used on AArch64 macOS.
    Aarch64AapcsDarwin,
}

/// How a psABI lays a stacked (byval / register-overflow) argument out in the
/// outgoing argument area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StackedArgumentPacking {
    /// Every stacked argument starts at the next 8-byte boundary and occupies a
    /// whole multiple of 8 bytes (SysV AMD64 §3.2.3, AAPCS64 §6.4.2 with its
    /// 8-byte stack-slot granule).
    EightByteSlots,
    /// Every stacked argument occupies its natural size at its natural
    /// alignment, packed against its predecessor: Apple's arm64 amendment, so a
    /// stacked `i8` occupies one byte and a stacked `i16` starts at the next
    /// even offset.
    NaturalSize,
}

/// Which physical register bank a value class travels in.
///
/// The two banks exist on every target Rue supports: general-purpose integer
/// and pointer registers, and the floating-point/SIMD file. The rosters are
/// sized per convention by [`CConventionSpec`]; the register *names* live in the
/// backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CRegisterClass {
    /// A general-purpose integer or pointer register.
    Gp,
    /// A floating-point / SIMD register (SysV's SSE class, AAPCS64's V bank).
    Fp,
}

/// Where a psABI puts the hidden indirect-result (sret) pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SretRegisterKind {
    /// An ordinary integer argument register: the pointer is the hidden first
    /// argument and shifts every user argument one register right (SysV AMD64
    /// `rdi`).
    ArgumentRegister,
    /// A register outside the ordinary argument roster, so user arguments still
    /// start at roster index 0 (AAPCS64 `x8`, section 6.9).
    DedicatedRegister,
}

/// Who is responsible for extending an integer narrower than its register.
///
/// Rue's internal invariant keeps every scalar canonically extended to 64 bits,
/// which is at least as strong as any row asks of an argument. The rows differ
/// only in what a *callee* may assume, which is what this records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NarrowIntegerExtension {
    /// The bits above a narrow argument's declared width are unspecified, and a
    /// callee that needs a wider value re-extends. A narrow *return* value's
    /// high bits are likewise not the caller's to trust (SysV AMD64 leaves them
    /// undefined; AAPCS64 defines only bits 0..31), so a caller re-extends from
    /// the value's own declared width.
    CalleeExtendsOnUse,
    /// The caller must extend an argument narrower than 32 bits before it
    /// crosses (Apple's arm64 amendment). Rue's canonical 64-bit form already
    /// satisfies it, so this row needs no extra instruction on the import side.
    CallerExtendsBelow32,
}

/// Which psABI rule classifies a by-value aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AggregateClassificationRule {
    /// SysV AMD64 section 3.2.3: classify each eightbyte, pass an aggregate of
    /// at most two eightbytes in registers of the classified banks, and pass a
    /// MEMORY class aggregate by value in the outgoing stack argument area,
    /// consuming no registers.
    SysVEightbyte,
    /// AAPCS64 section 6.8.2: a composite of at most 16 bytes travels in
    /// consecutive integer registers; a larger one is passed as a pointer to a
    /// caller-owned copy (B.4 / C.12), which is *not* the SysV byval-on-stack
    /// rule.
    Aapcs64Composite,
}

/// Everything the compiler needs to know about one platform C convention.
///
/// This is the data the classifier and both backends read instead of matching on
/// a [`CallingConvention`] row. A new row -- Win64, say -- is a new entry in
/// [`CallingConvention::c_spec`] and nothing else: `shadow_space_bytes` is 0 on
/// every current row precisely so that entry has a field to fill in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CConventionSpec {
    /// How many general-purpose registers carry arguments before the outgoing
    /// stack area begins (6 on SysV AMD64, 8 on AAPCS64).
    pub gp_argument_registers: u32,
    /// How many floating-point registers carry arguments (8 on every current
    /// row). Unreached while the C boundary rejects floats; carried so the
    /// float slice extends this table rather than rewriting it.
    pub fp_argument_registers: u32,
    /// How many general-purpose registers carry a result (`rax:rdx` on SysV
    /// AMD64, `x0:x1` on AAPCS64).
    pub gp_return_registers: u32,
    /// How many floating-point registers carry a result (`xmm0:xmm1` on SysV
    /// AMD64; `v0`-`v3` on AAPCS64, whose four-element HFA is the widest
    /// float-classed result).
    pub fp_return_registers: u32,
    /// Where the hidden indirect-result pointer travels.
    pub sret_register: SretRegisterKind,
    /// Whether the callee must leave the hidden indirect-result pointer in the
    /// primary result register on return (SysV AMD64 requires the `rax` echo;
    /// AAPCS64's `x8` is not echoed).
    pub sret_pointer_echoed_in_result_register: bool,
    /// The stack alignment required at a `call` instruction.
    pub call_stack_alignment: u32,
    /// Bytes the caller reserves at the base of the outgoing argument area for
    /// the callee's own use before the first stacked argument. Zero on every
    /// current row; Win64's 32-byte register spill area is the reason the field
    /// exists.
    pub shadow_space_bytes: u32,
    /// How the outgoing argument area is packed.
    pub stacked_argument_packing: StackedArgumentPacking,
    /// Who extends an integer narrower than its register.
    pub narrow_integer_extension: NarrowIntegerExtension,
    /// Which by-value aggregate rule applies.
    pub aggregate_rule: AggregateClassificationRule,
    /// The largest aggregate, in bytes, that crosses in registers rather than
    /// through memory. Two eightbytes on both current rows.
    pub max_aggregate_register_bytes: u64,
}

impl CConventionSpec {
    /// The number of argument registers in `class`.
    pub const fn argument_registers(&self, class: CRegisterClass) -> u32 {
        match class {
            CRegisterClass::Gp => self.gp_argument_registers,
            CRegisterClass::Fp => self.fp_argument_registers,
        }
    }

    /// The number of result registers in `class`.
    pub const fn return_registers(&self, class: CRegisterClass) -> u32 {
        match class {
            CRegisterClass::Gp => self.gp_return_registers,
            CRegisterClass::Fp => self.fp_return_registers,
        }
    }

    /// Whether the hidden indirect-result pointer consumes the first ordinary
    /// integer argument register, shifting user arguments right.
    pub const fn sret_pointer_in_argument_register(&self) -> bool {
        matches!(self.sret_register, SretRegisterKind::ArgumentRegister)
    }
}

/// The SysV AMD64 row (x86-64 Linux).
const X86_64_SYSV: CConventionSpec = CConventionSpec {
    gp_argument_registers: 6,
    fp_argument_registers: 8,
    gp_return_registers: 2,
    fp_return_registers: 2,
    sret_register: SretRegisterKind::ArgumentRegister,
    sret_pointer_echoed_in_result_register: true,
    call_stack_alignment: 16,
    shadow_space_bytes: 0,
    stacked_argument_packing: StackedArgumentPacking::EightByteSlots,
    narrow_integer_extension: NarrowIntegerExtension::CalleeExtendsOnUse,
    aggregate_rule: AggregateClassificationRule::SysVEightbyte,
    max_aggregate_register_bytes: 16,
};

/// The AAPCS64 row (AArch64 Linux).
const AARCH64_AAPCS: CConventionSpec = CConventionSpec {
    gp_argument_registers: 8,
    fp_argument_registers: 8,
    gp_return_registers: 2,
    fp_return_registers: 4,
    sret_register: SretRegisterKind::DedicatedRegister,
    sret_pointer_echoed_in_result_register: false,
    call_stack_alignment: 16,
    shadow_space_bytes: 0,
    stacked_argument_packing: StackedArgumentPacking::EightByteSlots,
    narrow_integer_extension: NarrowIntegerExtension::CalleeExtendsOnUse,
    aggregate_rule: AggregateClassificationRule::Aapcs64Composite,
    max_aggregate_register_bytes: 16,
};

/// The Apple arm64 row (AArch64 macOS): AAPCS64 with Apple's two amendments
/// Rue's supported surface reaches.
const AARCH64_AAPCS_DARWIN: CConventionSpec = CConventionSpec {
    stacked_argument_packing: StackedArgumentPacking::NaturalSize,
    narrow_integer_extension: NarrowIntegerExtension::CallerExtendsBelow32,
    ..AARCH64_AAPCS
};

impl CallingConvention {
    /// The convention a `"C"` boundary follows on `target`.
    ///
    /// This is the single `"C"` alias table. It is keyed by the whole target,
    /// not by its architecture: AArch64 Linux and AArch64 macOS share an
    /// architecture and do not share a convention.
    pub const fn c_for_target(target: Target) -> Self {
        match target {
            Target::X86_64Linux => Self::X86_64SysV,
            Target::Aarch64Linux => Self::Aarch64Aapcs,
            Target::Aarch64Macos => Self::Aarch64AapcsDarwin,
        }
    }

    /// Whether this is the native Rue convention.
    pub const fn is_rue(self) -> bool {
        matches!(self, Self::Rue)
    }

    /// Whether this is a platform C convention — the complement of
    /// [`is_rue`](Self::is_rue), spelled for call sites that read better as a
    /// positive.
    pub const fn is_c(self) -> bool {
        !self.is_rue()
    }

    /// This convention's complete psABI description. Panics on
    /// [`CallingConvention::Rue`], which is not a psABI: the native convention's
    /// rules are the classifier's own (`rue_air::NativeCallAbi`), not a row of
    /// this table.
    pub const fn c_spec(self) -> CConventionSpec {
        match self {
            Self::X86_64SysV => X86_64_SYSV,
            Self::Aarch64Aapcs => AARCH64_AAPCS,
            Self::Aarch64AapcsDarwin => AARCH64_AAPCS_DARWIN,
            Self::Rue => panic!(
                "the native Rue convention is not a psABI row; its rules live in \
                 the native call-ABI classifier"
            ),
        }
    }

    /// How a stacked argument is laid out in the outgoing argument area under
    /// this convention: the [`CConventionSpec`] field, spelled as a method for
    /// the placement code that reads only it. Panics on
    /// [`CallingConvention::Rue`], whose stack arguments are the native uniform
    /// 8-byte slot model rather than a psABI argument area.
    pub const fn stacked_argument_packing(self) -> StackedArgumentPacking {
        self.c_spec().stacked_argument_packing
    }

    /// The canonical name of this convention, for diagnostics and dumps.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rue => "rue",
            Self::X86_64SysV => "x86-64-sysv",
            Self::Aarch64Aapcs => "aarch64-aapcs",
            Self::Aarch64AapcsDarwin => "aarch64-aapcs-darwin",
        }
    }

    /// The convention an `extern` ABI string names outright, or `None` when the
    /// string names no foreign convention.
    ///
    /// This is [`name`](Self::name) read backwards, restricted to the psABI
    /// rows: `"rue"` does not parse, because the native convention is not a
    /// foreign boundary and an `extern` declaration naming it would describe no
    /// crossing. `"C"` does not parse here either — it is not a convention but
    /// an alias for one, which [`ForeignAbi`] resolves against a target.
    pub fn parse_abi_string(name: &str) -> Option<Self> {
        Self::C_ROWS
            .into_iter()
            .find(|convention| convention.name() == name)
    }

    /// The platform C conventions, in the order diagnostics list them. The
    /// native row is deliberately absent: every member of this table is a
    /// foreign boundary.
    const C_ROWS: [Self; 3] = [
        Self::X86_64SysV,
        Self::Aarch64Aapcs,
        Self::Aarch64AapcsDarwin,
    ];

    /// Whether `target` implements this convention.
    ///
    /// Answered from the one `"C"` alias table rather than a second list, so a
    /// new target answers this question by answering that one. The native row is
    /// implemented everywhere; each psABI row belongs to the single target whose
    /// C boundary follows it.
    pub fn is_implemented_by(self, target: Target) -> bool {
        self.is_rue() || Self::c_for_target(target) == self
    }

    /// Every target that implements this convention, in [`Target::all`] order.
    /// Diagnostics use it to say which target a rejected convention belongs to.
    pub fn implementing_targets(self) -> impl Iterator<Item = Target> {
        Target::all()
            .iter()
            .copied()
            .filter(move |target| self.is_implemented_by(*target))
    }
}

/// The calling convention an `extern` declaration names in source.
///
/// A foreign declaration writes either the alias `"C"`, which denotes whichever
/// psABI the compilation target's C boundary follows, or one convention's own
/// [`CallingConvention::name`] spelling, which denotes that row outright. The
/// two forms stay apart only up to resolution: [`resolve`](Self::resolve) is the
/// single place a declaration's written ABI becomes a [`CallingConvention`], and
/// everything past it carries the row rather than the string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ForeignAbi {
    /// `"C"`: the compilation target's own C convention.
    C,
    /// A psABI row named outright by its canonical name.
    Explicit(CallingConvention),
}

impl ForeignAbi {
    /// The ABI string that spells the target-C alias.
    pub const C_ABI_STRING: &'static str = "C";

    /// The declaration ABI `text` names, or `None` when no foreign boundary is
    /// spelled that way.
    ///
    /// The accepted set is exactly `"C"` plus the psABI rows' own names, so the
    /// spellings a declaration may write and the spellings `--emit abi` and
    /// diagnostics print come from one table. `"C-unwind"` stays reserved and
    /// does not parse (9.3:2), and neither does `"rue"`.
    pub fn parse(text: &str) -> Option<Self> {
        if text == Self::C_ABI_STRING {
            return Some(Self::C);
        }
        CallingConvention::parse_abi_string(text).map(Self::Explicit)
    }

    /// Every ABI string a foreign declaration may write, in the order
    /// diagnostics list them.
    pub fn accepted_abi_strings() -> impl Iterator<Item = &'static str> {
        std::iter::once(Self::C_ABI_STRING).chain(
            CallingConvention::C_ROWS
                .into_iter()
                .map(CallingConvention::name),
        )
    }

    /// The convention this declaration follows when compiled for `target`.
    ///
    /// This is the one place the `"C"` alias is resolved on a declaration's
    /// behalf; an explicit row resolves to itself. Resolving does not ask
    /// whether `target` implements the result —
    /// [`is_implemented_by`](Self::is_implemented_by) is that question, and
    /// 9.3:1b makes a declaration naming an unimplemented row ill-formed.
    pub const fn resolve(self, target: Target) -> CallingConvention {
        match self {
            Self::C => CallingConvention::c_for_target(target),
            Self::Explicit(convention) => convention,
        }
    }

    /// The source spelling of this ABI: `"C"`, or the row's canonical name.
    /// The inverse of [`parse`](Self::parse), which is what makes the written
    /// spelling and the printed one one table.
    pub const fn abi_string(self) -> &'static str {
        match self {
            Self::C => Self::C_ABI_STRING,
            Self::Explicit(convention) => convention.name(),
        }
    }
}

impl core::fmt::Display for CallingConvention {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

impl Target {
    /// The convention this target's `"C"` boundary follows; the method spelling
    /// of [`CallingConvention::c_for_target`].
    pub const fn c_calling_convention(self) -> CallingConvention {
        CallingConvention::c_for_target(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_c_alias_is_total_and_distinct_over_every_target() {
        let mut seen = Vec::new();
        for target in Target::all() {
            let convention = target.c_calling_convention();
            assert_eq!(
                convention,
                CallingConvention::c_for_target(*target),
                "the method and the free function must name one table"
            );
            assert!(convention.is_c(), "a C boundary is never the Rue row");
            assert!(
                !seen.contains(&convention),
                "each target names its own psABI row: {convention} repeated"
            );
            seen.push(convention);
        }
        assert_eq!(seen.len(), Target::all().len());
    }

    #[test]
    fn aarch64_targets_share_an_architecture_and_not_a_convention() {
        assert_eq!(
            Target::Aarch64Linux.arch(),
            Target::Aarch64Macos.arch(),
            "the two AArch64 targets share an architecture"
        );
        assert_ne!(
            Target::Aarch64Linux.c_calling_convention(),
            Target::Aarch64Macos.c_calling_convention(),
            "the `\"C\"` alias is keyed by target, not architecture"
        );
        assert_eq!(
            Target::Aarch64Macos.c_calling_convention(),
            CallingConvention::Aarch64AapcsDarwin
        );
    }

    #[test]
    fn only_darwin_packs_stacked_arguments_at_their_natural_size() {
        assert_eq!(
            CallingConvention::X86_64SysV.stacked_argument_packing(),
            StackedArgumentPacking::EightByteSlots
        );
        assert_eq!(
            CallingConvention::Aarch64Aapcs.stacked_argument_packing(),
            StackedArgumentPacking::EightByteSlots
        );
        assert_eq!(
            CallingConvention::Aarch64AapcsDarwin.stacked_argument_packing(),
            StackedArgumentPacking::NaturalSize
        );
    }

    #[test]
    fn every_c_row_has_a_complete_convention_description() {
        for target in Target::all() {
            let spec = target.c_calling_convention().c_spec();
            assert!(
                spec.gp_argument_registers > 0 && spec.gp_return_registers > 0,
                "{target:?} must name a general-purpose roster"
            );
            assert!(
                spec.fp_argument_registers > 0 && spec.fp_return_registers > 0,
                "{target:?} must size its floating-point rosters so the float \
                 boundary extends this table instead of rewriting it"
            );
            assert_eq!(
                spec.call_stack_alignment, 16,
                "every supported psABI aligns the stack to 16 bytes at a call"
            );
            assert_eq!(
                spec.shadow_space_bytes, 0,
                "no supported row reserves callee shadow space"
            );
            assert_eq!(spec.max_aggregate_register_bytes, 16);
            assert_eq!(
                spec.argument_registers(CRegisterClass::Gp),
                spec.gp_argument_registers
            );
            assert_eq!(
                spec.return_registers(CRegisterClass::Fp),
                spec.fp_return_registers
            );
        }
    }

    #[test]
    fn the_two_psabi_families_differ_exactly_where_the_specifications_do() {
        let sysv = CallingConvention::X86_64SysV.c_spec();
        let aapcs = CallingConvention::Aarch64Aapcs.c_spec();
        assert_eq!(sysv.gp_argument_registers, 6);
        assert_eq!(aapcs.gp_argument_registers, 8);
        // SysV passes the hidden result pointer as the hidden first argument in
        // `rdi` and requires the `rax` echo; AAPCS64 uses the dedicated `x8`.
        assert_eq!(sysv.sret_register, SretRegisterKind::ArgumentRegister);
        assert!(sysv.sret_pointer_in_argument_register());
        assert!(sysv.sret_pointer_echoed_in_result_register);
        assert_eq!(aapcs.sret_register, SretRegisterKind::DedicatedRegister);
        assert!(!aapcs.sret_pointer_in_argument_register());
        assert!(!aapcs.sret_pointer_echoed_in_result_register);
        assert_eq!(
            sysv.aggregate_rule,
            AggregateClassificationRule::SysVEightbyte
        );
        assert_eq!(
            aapcs.aggregate_rule,
            AggregateClassificationRule::Aapcs64Composite
        );
    }

    #[test]
    fn the_apple_row_amends_exactly_two_aapcs64_fields() {
        let aapcs = CallingConvention::Aarch64Aapcs.c_spec();
        let darwin = CallingConvention::Aarch64AapcsDarwin.c_spec();
        assert_eq!(
            darwin.stacked_argument_packing,
            StackedArgumentPacking::NaturalSize
        );
        assert_eq!(
            darwin.narrow_integer_extension,
            NarrowIntegerExtension::CallerExtendsBelow32
        );
        // Everything else is AAPCS64 verbatim, which the struct update syntax
        // guarantees and this pins against a hand-edited row drifting.
        assert_eq!(
            CConventionSpec {
                stacked_argument_packing: aapcs.stacked_argument_packing,
                narrow_integer_extension: aapcs.narrow_integer_extension,
                ..darwin
            },
            aapcs
        );
    }

    #[test]
    #[should_panic(expected = "not a psABI row")]
    fn the_native_row_has_no_psabi_description() {
        let _ = CallingConvention::Rue.c_spec();
    }

    #[test]
    fn every_c_row_parses_back_from_its_own_name() {
        for target in Target::all() {
            let convention = target.c_calling_convention();
            assert_eq!(
                CallingConvention::parse_abi_string(convention.name()),
                Some(convention),
                "the ABI string table and the name table must be one table"
            );
            assert_eq!(
                ForeignAbi::parse(convention.name()),
                Some(ForeignAbi::Explicit(convention))
            );
        }
    }

    #[test]
    fn the_native_convention_is_not_a_foreign_abi() {
        assert_eq!(CallingConvention::Rue.name(), "rue");
        assert_eq!(CallingConvention::parse_abi_string("rue"), None);
        assert_eq!(ForeignAbi::parse("rue"), None);
    }

    #[test]
    fn only_the_declared_abi_spellings_parse() {
        assert_eq!(ForeignAbi::parse("C"), Some(ForeignAbi::C));
        for rejected in ["C-unwind", "Rust", "system", "c", "sysv64", ""] {
            assert_eq!(
                ForeignAbi::parse(rejected),
                None,
                "{rejected} must not parse"
            );
        }
        assert_eq!(
            ForeignAbi::accepted_abi_strings().collect::<Vec<_>>(),
            vec!["C", "x86-64-sysv", "aarch64-aapcs", "aarch64-aapcs-darwin"]
        );
        for text in ForeignAbi::accepted_abi_strings() {
            assert_eq!(
                ForeignAbi::parse(text).map(ForeignAbi::abi_string),
                Some(text),
                "every listed spelling round-trips"
            );
        }
    }

    #[test]
    fn the_c_alias_resolves_per_target_and_an_explicit_row_resolves_to_itself() {
        for target in Target::all() {
            assert_eq!(
                ForeignAbi::C.resolve(*target),
                target.c_calling_convention()
            );
            assert!(
                ForeignAbi::C.resolve(*target).is_implemented_by(*target),
                "the alias is implemented by every target by construction"
            );
        }
        assert_eq!(
            ForeignAbi::Explicit(CallingConvention::Aarch64Aapcs).resolve(Target::X86_64Linux),
            CallingConvention::Aarch64Aapcs,
            "resolution never falls back to the target's own row"
        );
    }

    #[test]
    fn each_c_row_is_implemented_by_exactly_one_target() {
        for target in Target::all() {
            let convention = target.c_calling_convention();
            assert_eq!(
                convention.implementing_targets().collect::<Vec<_>>(),
                vec![*target]
            );
            for other in Target::all() {
                assert_eq!(
                    ForeignAbi::Explicit(convention)
                        .resolve(*other)
                        .is_implemented_by(*other),
                    other == target,
                    "a named row never resolves to the compiling target's own"
                );
            }
        }
        assert!(
            Target::all()
                .iter()
                .all(|target| CallingConvention::Rue.is_implemented_by(*target)),
            "the native row is not a foreign boundary and is never target-rejected"
        );
    }

    #[test]
    fn the_rue_row_is_the_only_native_convention() {
        assert!(CallingConvention::Rue.is_rue());
        assert!(!CallingConvention::Rue.is_c());
        for convention in [
            CallingConvention::X86_64SysV,
            CallingConvention::Aarch64Aapcs,
            CallingConvention::Aarch64AapcsDarwin,
        ] {
            assert!(!convention.is_rue());
            assert!(convention.is_c());
        }
    }
}
