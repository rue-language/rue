//! The register bank and move width one ABI-placed value travels in.
//!
//! This is a **code-generation detail, not a convention fact**. Where a value
//! crosses is `rue_air::lower_c_signature` /
//! `rue_air::lower_native_signature`'s answer, and that answer names a bank as
//! [`rue_target::CRegisterClass`]. What the backends additionally need is the
//! *width* of the move that puts the value there: an `f32` leaf and an `f64`
//! leaf both travel in the floating-point file, but they are moved by different
//! instructions. This type is that pair — a bank plus, for the floating-point
//! bank, the leaf's own width — and it exists so both backends, the parameter
//! homing plan, and the `--emit abi` reporter select a register file the same
//! way.
//!
//! It has exactly one owner (this module) and one construction rule
//! ([`AbiSlotClass::for_leaf`]); nothing outside `rue-codegen` sees it.

use rue_air::Type;
use rue_target::CRegisterClass;

use crate::value_plan::FloatWidth;

/// The register file, and the width of the move, one placed value uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiSlotClass {
    /// The general-purpose file, moved at register width.
    Gp,
    /// The floating-point file, moved at this width.
    Fp(FloatWidth),
}

impl AbiSlotClass {
    /// The class of a value carrying `leaf`, the one rule every direction of a
    /// crossing uses: a float leaf goes to the FP bank, at its own width;
    /// everything else to the GP bank.
    pub fn for_leaf(leaf: Type) -> Self {
        match crate::value_plan::float_width(leaf) {
            Some(width) => Self::Fp(width),
            None => Self::Gp,
        }
    }

    /// The bank the classifier named this value's placement in.
    pub const fn bank(self) -> CRegisterClass {
        match self {
            Self::Gp => CRegisterClass::Gp,
            Self::Fp(_) => CRegisterClass::Fp,
        }
    }
}
