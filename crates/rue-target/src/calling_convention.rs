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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    /// How a stacked argument is laid out in the outgoing argument area under
    /// this convention. Panics on [`CallingConvention::Rue`], whose stack
    /// arguments are the native uniform 8-byte slot model rather than a psABI
    /// argument area.
    pub const fn stacked_argument_packing(self) -> StackedArgumentPacking {
        match self {
            Self::X86_64SysV | Self::Aarch64Aapcs => StackedArgumentPacking::EightByteSlots,
            Self::Aarch64AapcsDarwin => StackedArgumentPacking::NaturalSize,
            Self::Rue => panic!(
                "the native Rue convention has no psABI outgoing argument area; \
                 stacked_argument_packing is a target-C question"
            ),
        }
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
