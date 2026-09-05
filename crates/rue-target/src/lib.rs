//! Target architecture and OS definitions for the Rue compiler.
//!
//! This crate provides the `Target` enum and related types that define
//! compilation targets, including [`CallingConvention`] — the one value type
//! for "which calling convention governs this boundary" — and the `"C"` alias
//! table that resolves it from a target. It is a leaf crate with no
//! dependencies, designed to be used by the CLI, compiler, codegen, and linker
//! crates.

mod calling_convention;

pub use calling_convention::{
    AggregateClassificationRule, CConventionSpec, CRegisterClass, CallingConvention, ForeignAbi,
    NarrowIntegerExtension, SretRegisterKind, StackedArgumentPacking,
};

use std::fmt;
use std::str::FromStr;

/// A compilation target consisting of an architecture and operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// x86-64 Linux (System V AMD64 ABI)
    X86_64Linux,
    /// AArch64 Linux (AAPCS64 ABI)
    Aarch64Linux,
    /// AArch64 macOS (Apple Silicon, AAPCS64 with Apple extensions)
    Aarch64Macos,
}

impl Target {
    /// Detect the host target at compile time.
    ///
    /// Returns `None` when the current compilation environment is not a
    /// supported Rue target. Callers that need a default target must handle
    /// that case explicitly instead of silently compiling for some other
    /// platform.
    pub fn host() -> Option<Self> {
        if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
            Some(Target::X86_64Linux)
        } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
            Some(Target::Aarch64Linux)
        } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
            Some(Target::Aarch64Macos)
        } else {
            None
        }
    }

    /// Returns a best-effort name for the host this compiler binary was built
    /// for, including unsupported hosts. Used in diagnostics.
    pub fn host_description() -> &'static str {
        if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
            "x86-64-linux"
        } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
            "aarch64-linux"
        } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
            "aarch64-macos"
        } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
            "x86-64-macos"
        } else {
            "unsupported host"
        }
    }

    /// Returns the architecture component of this target.
    pub fn arch(&self) -> Arch {
        match self {
            Target::X86_64Linux => Arch::X86_64,
            Target::Aarch64Linux | Target::Aarch64Macos => Arch::Aarch64,
        }
    }

    /// Returns the operating system component of this target.
    pub fn os(&self) -> Os {
        match self {
            Target::X86_64Linux | Target::Aarch64Linux => Os::Linux,
            Target::Aarch64Macos => Os::Macos,
        }
    }

    /// Returns the C data model of this target: the widths the platform psABI
    /// assigns to C `int`, `long`, and pointers (ADR-0064 Amendment 1).
    ///
    /// Architecture alone does not fix the data model — AAPCS64 is the worked
    /// example: an AArch64 target can be LP64 (Linux, macOS) or LLP64 — so the
    /// data model is a named target fact. Every currently-supported target is
    /// LP64 (`int` = 32, `long` = 64, pointer = 64). It is exposed to Rue source
    /// through the `@target_data_model()` intrinsic, mirroring `@target_arch()`
    /// / `@target_os()`, so `std.c`'s width-dependent aliases can eventually be
    /// selected per target once const-context target branching exists.
    pub fn data_model(&self) -> DataModel {
        match self {
            // SysV AMD64, AAPCS64 (Linux), and the Apple ARM64 platform ABI are
            // all LP64.
            Target::X86_64Linux | Target::Aarch64Linux | Target::Aarch64Macos => DataModel::Lp64,
        }
    }

    /// Returns the ELF e_machine value for this target, if it uses ELF format.
    ///
    /// This is used when generating ELF object files and executables.
    /// Returns `None` for targets that don't use ELF (e.g., macOS uses Mach-O).
    pub fn elf_machine(&self) -> Option<u16> {
        if !self.is_elf() {
            return None;
        }
        match self.arch() {
            Arch::X86_64 => Some(0x3E),  // EM_X86_64
            Arch::Aarch64 => Some(0xB7), // EM_AARCH64
        }
    }

    /// Returns the default page size for this target in bytes.
    ///
    /// This is used for executable segment alignment.
    pub const fn page_size(&self) -> u64 {
        match self {
            // x86-64 and AArch64 Linux typically use 4KB pages.
            Target::X86_64Linux | Target::Aarch64Linux => 0x1000, // 4KB
            // macOS on Apple Silicon uses 16KB pages.
            Target::Aarch64Macos => 0x4000, // 16KB
        }
    }

    /// Returns the default base address for executables on this target.
    ///
    /// This is the virtual address where the executable is loaded.
    pub const fn default_base_addr(&self) -> u64 {
        match self {
            // Standard Linux load address for both architectures.
            Target::X86_64Linux | Target::Aarch64Linux => 0x400000,
            // macOS uses a different address space layout; the dynamic linker
            // handles placement. We use a conventional address.
            Target::Aarch64Macos => 0x100000000,
        }
    }

    /// Returns whether this target uses Mach-O object format (macOS).
    pub fn is_macho(&self) -> bool {
        matches!(self, Target::Aarch64Macos)
    }

    /// Returns whether this target uses ELF object format (Linux).
    pub fn is_elf(&self) -> bool {
        matches!(self, Target::X86_64Linux | Target::Aarch64Linux)
    }

    /// Returns the minimum macOS version for this target, encoded for Mach-O.
    ///
    /// The version is encoded as `0x00XXYYPP` where XX is major, YY is minor, PP is patch.
    /// For example, macOS 11.0.0 (Big Sur) is encoded as `0x000B0000`.
    ///
    /// Returns `None` for non-macOS targets.
    ///
    /// Note: macOS 11.0 (Big Sur) was the first version to support Apple Silicon (ARM64),
    /// which is why it's the minimum for `Aarch64Macos`.
    pub fn macos_min_version(&self) -> Option<u32> {
        match self {
            Target::Aarch64Macos => Some(0x000B0000), // 11.0.0 (Big Sur)
            Target::X86_64Linux | Target::Aarch64Linux => None,
        }
    }

    /// Returns all supported targets.
    pub fn all() -> &'static [Target] {
        &[
            Target::X86_64Linux,
            Target::Aarch64Linux,
            Target::Aarch64Macos,
        ]
    }

    /// Returns a comma-separated string of all target names for help text.
    pub fn all_names() -> &'static str {
        "x86-64-linux, aarch64-linux, aarch64-macos"
    }

    /// Returns the canonical Rue spelling of this target.
    pub const fn name(&self) -> &'static str {
        match self {
            Target::X86_64Linux => "x86-64-linux",
            Target::Aarch64Linux => "aarch64-linux",
            Target::Aarch64Macos => "aarch64-macos",
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Error returned when parsing an invalid target string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTargetError {
    input: String,
}

impl fmt::Display for ParseTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown target '{}'. Valid targets: {}",
            self.input,
            Target::all()
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for ParseTargetError {}

impl Default for Target {
    /// Returns the host target as the default.
    ///
    /// This allows code to write `Target::default()` instead of `Target::host()`,
    /// which is useful for struct initialization with `..Default::default()`.
    fn default() -> Self {
        Self::host().expect("Rue cannot choose a default target on this unsupported host")
    }
}

impl FromStr for Target {
    type Err = ParseTargetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "x86-64-linux" | "x86_64-linux" | "x86_64-unknown-linux-gnu" => Ok(Target::X86_64Linux),
            "aarch64-linux" | "arm64-linux" | "aarch64-unknown-linux-gnu" => {
                Ok(Target::Aarch64Linux)
            }
            "aarch64-macos" | "arm64-macos" | "aarch64-apple-darwin" => Ok(Target::Aarch64Macos),
            _ => Err(ParseTargetError {
                input: s.to_string(),
            }),
        }
    }
}

/// The CPU architecture of a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    /// x86-64 (AMD64)
    X86_64,
    /// AArch64 (ARM64)
    Aarch64,
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Arch::X86_64 => write!(f, "x86-64"),
            Arch::Aarch64 => write!(f, "aarch64"),
        }
    }
}

/// The operating system of a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    /// Linux
    Linux,
    /// macOS (Darwin)
    Macos,
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Os::Linux => write!(f, "linux"),
            Os::Macos => write!(f, "macos"),
        }
    }
}

/// The C data model of a target: the widths the platform psABI assigns to the
/// standard C integer and pointer types (ADR-0064 Amendment 1).
///
/// The data model is the compact way psABIs name their scalar-width choices.
/// It is orthogonal to architecture (an AArch64 target may be LP64 or LLP64),
/// which is why the target descriptor carries it as a distinct fact. Variant
/// order is stable: it fixes the `@target_data_model()` intrinsic's enum
/// discriminants (0 = ILP32, 1 = LP64, 2 = LLP64).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataModel {
    /// `int` = 32, `long` = 32, pointer = 32 (32-bit Unix, Win32).
    Ilp32,
    /// `int` = 32, `long` = 64, pointer = 64 (64-bit Unix: Linux, macOS).
    Lp64,
    /// `int` = 32, `long` = 32, `long long` = 64, pointer = 64 (64-bit Windows).
    Llp64,
}

impl fmt::Display for DataModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataModel::Ilp32 => write!(f, "ilp32"),
            DataModel::Lp64 => write!(f, "lp64"),
            DataModel::Llp64 => write!(f, "llp64"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_parsing() {
        assert_eq!(
            "x86-64-linux".parse::<Target>().unwrap(),
            Target::X86_64Linux
        );
        assert_eq!(
            "x86_64-linux".parse::<Target>().unwrap(),
            Target::X86_64Linux
        );
        assert_eq!(
            "aarch64-linux".parse::<Target>().unwrap(),
            Target::Aarch64Linux
        );
        assert_eq!(
            "arm64-linux".parse::<Target>().unwrap(),
            Target::Aarch64Linux
        );
        assert_eq!(
            "aarch64-macos".parse::<Target>().unwrap(),
            Target::Aarch64Macos
        );
        assert_eq!(
            "arm64-macos".parse::<Target>().unwrap(),
            Target::Aarch64Macos
        );
        assert_eq!(
            "aarch64-apple-darwin".parse::<Target>().unwrap(),
            Target::Aarch64Macos
        );
    }

    #[test]
    fn test_target_display() {
        assert_eq!(Target::X86_64Linux.to_string(), "x86-64-linux");
        assert_eq!(Target::Aarch64Linux.to_string(), "aarch64-linux");
        assert_eq!(Target::Aarch64Macos.to_string(), "aarch64-macos");
    }

    #[test]
    fn test_invalid_target() {
        assert!("windows".parse::<Target>().is_err());
        assert!("riscv64".parse::<Target>().is_err());
    }

    #[test]
    fn test_elf_machine() {
        assert_eq!(Target::X86_64Linux.elf_machine(), Some(0x3E));
        assert_eq!(Target::Aarch64Linux.elf_machine(), Some(0xB7));
        // Mach-O targets return None since they don't use ELF format
        assert_eq!(Target::Aarch64Macos.elf_machine(), None);
    }

    #[test]
    fn test_arch_decomposition() {
        assert_eq!(Target::X86_64Linux.arch(), Arch::X86_64);
        assert_eq!(Target::Aarch64Linux.arch(), Arch::Aarch64);
        assert_eq!(Target::Aarch64Macos.arch(), Arch::Aarch64);
    }

    #[test]
    fn test_os_decomposition() {
        assert_eq!(Target::X86_64Linux.os(), Os::Linux);
        assert_eq!(Target::Aarch64Linux.os(), Os::Linux);
        assert_eq!(Target::Aarch64Macos.os(), Os::Macos);
    }

    #[test]
    fn test_data_model_decomposition() {
        // Every currently-supported target is LP64.
        assert_eq!(Target::X86_64Linux.data_model(), DataModel::Lp64);
        assert_eq!(Target::Aarch64Linux.data_model(), DataModel::Lp64);
        assert_eq!(Target::Aarch64Macos.data_model(), DataModel::Lp64);
    }

    #[test]
    fn test_data_model_discriminants_are_stable() {
        // The `@target_data_model()` intrinsic maps these to enum variant
        // indices; the order must not drift.
        assert_eq!(DataModel::Ilp32 as u8, 0);
        assert_eq!(DataModel::Lp64 as u8, 1);
        assert_eq!(DataModel::Llp64 as u8, 2);
        assert_eq!(DataModel::Ilp32.to_string(), "ilp32");
        assert_eq!(DataModel::Lp64.to_string(), "lp64");
        assert_eq!(DataModel::Llp64.to_string(), "llp64");
    }

    #[test]
    fn test_is_elf_macho() {
        assert!(Target::X86_64Linux.is_elf());
        assert!(Target::Aarch64Linux.is_elf());
        assert!(!Target::Aarch64Macos.is_elf());

        assert!(!Target::X86_64Linux.is_macho());
        assert!(!Target::Aarch64Linux.is_macho());
        assert!(Target::Aarch64Macos.is_macho());
    }

    #[test]
    fn test_page_size() {
        assert_eq!(Target::X86_64Linux.page_size(), 0x1000);
        assert_eq!(Target::Aarch64Linux.page_size(), 0x1000);
        assert_eq!(Target::Aarch64Macos.page_size(), 0x4000);
    }

    #[test]
    fn test_macos_min_version() {
        // Linux targets return None
        assert_eq!(Target::X86_64Linux.macos_min_version(), None);
        assert_eq!(Target::Aarch64Linux.macos_min_version(), None);
        // macOS returns the encoded version (11.0.0 = 0x000B0000 for Big Sur)
        assert_eq!(Target::Aarch64Macos.macos_min_version(), Some(0x000B0000));
    }

    #[test]
    fn test_default_returns_host() {
        assert_eq!(Target::default(), Target::host().unwrap());
    }

    #[test]
    fn test_host_description_present() {
        assert!(!Target::host_description().is_empty());
    }

    #[test]
    fn test_display_from_str_round_trip() {
        // Verify that Display and FromStr are inverses for all targets
        for target in Target::all() {
            let displayed = target.to_string();
            let parsed: Target = displayed
                .parse()
                .expect("Display output should be parseable");
            assert_eq!(
                *target, parsed,
                "Round-trip failed for {}: displayed as '{}', parsed back as {:?}",
                target, displayed, parsed
            );
        }
    }
}
