//! Rue-to-C export thunks (ADR-0064 P4).
//!
//! The mirror of the P1/P2/P3 foreign-call path. A foreign *call* adapts the
//! native convention to the target-C convention on the way *out* to a C callee;
//! an *export* thunk adapts the target-C convention to the native convention on
//! the way *in* from a C caller. A `pub extern "C" fn` is compiled like any other
//! Rue function (a native-conventioned body under a mangled symbol); this module
//! emits an additional, globally-visible, **unmangled** entry symbol — the C
//! symbol — whose body is a small prologue that receives arguments per the psABI
//! and forwards to the native body.
//!
//! ## Why scalars/pointers are a prologue, not a repack
//!
//! For the integer/pointer scalars this phase supports, the native Rue argument
//! convention and every target-C convention already agree on *where* each
//! argument lives: the first arguments occupy the same integer registers (`rdi,
//! rsi, rdx, rcx, r8, r9` on SysV; `x0..x7` on the AAPCS64 rows), a scalar
//! return comes back in the same primary result register, and Rue's internal
//! invariant keeps a scalar canonically extended — which is *stronger* than what
//! a C caller guarantees. The one place the conventions disagree is the
//! **narrow-integer extension**: a C caller may leave the bits above a narrow
//! argument's declared width unspecified (SysV), extended only to 32 bits
//! (AAPCS64), or extended to 32 bits by requirement (Apple arm64), whereas the
//! native body assumes the program-wide canonical 64-bit form. So the thunk
//! re-extends each narrow argument register in place — reusing the shared
//! [`TargetCCallAbi`] authority so it cannot disagree with the import side about
//! *which* extension a type needs — and then calls the native body. The scalar
//! return needs no fix-up: the native body already produces the canonical form a
//! C caller accepts (including a 0/1 `_Bool`).
//!
//! ## Abort at the boundary (ratified, ADR-0064 ruling 3)
//!
//! A trap that occurs while a C caller is on the stack must abort the process,
//! never unwind a C frame. Rue has **no unwinding machinery at all**: every trap
//! (overflow, bounds, `@panic`, failed checked assertion) lowers to a runtime
//! call that writes a diagnostic and performs a direct `exit(2)` syscall, and
//! `unreachable` lowers to an illegal instruction. Executing a native body
//! through this thunk therefore inherits abort-at-boundary for free — there is
//! no code path by which a trap could return into the thunk and propagate an
//! unwind into C. This module adds no guard because none is needed; the property
//! is structural, and the P4 CLI suite proves it by observing a trapping export
//! terminate a C caller with the runtime's deterministic exit status.
//!
//! Aggregate (`@repr(c)` struct/array) parameters and returns, and argument
//! lists wider than the shared register budget, are rejected in semantic
//! analysis (`ExportSignatureUnsupported`, E1106); this module only ever sees
//! register-resident scalar/pointer signatures.

use rue_air::{ScalarAbiExtension, TargetCCallAbi, Type};
use rue_target::{Arch, Target};

use crate::{EmittedRelocation, MachineCode};

/// Build the machine code for a Rue-to-C export thunk.
///
/// `native_symbol` is the mangled symbol of the natively-conventioned body the
/// thunk forwards to; `param_types` are the export's scalar parameter types in
/// source (argument-register) order. The returned [`MachineCode`] carries one
/// call/branch relocation targeting `native_symbol` and no string data.
pub fn generate_export_thunk(
    target: Target,
    native_symbol: &str,
    param_types: &[Type],
) -> MachineCode {
    match target.arch() {
        Arch::X86_64 => generate_x86_64(target, native_symbol, param_types),
        Arch::Aarch64 => generate_aarch64(target, native_symbol, param_types),
    }
}

/// The extension each argument register needs, in register order.
///
/// The convention comes from the target's `"C"` alias, not from its
/// architecture, so an AArch64 macOS export consults the Apple row. Both
/// directions of the C boundary consult the same authority
/// ([`TargetCCallAbi`]), so the export thunk and the import call agree on every
/// scalar extension.
///
/// Apple's arm64 ABI *requires* the caller to extend an argument narrower than
/// 32 bits, so a Darwin thunk could in principle trust the incoming register.
/// It re-extends anyway: SysV and generic AAPCS64 callers make no such promise,
/// the native body needs Rue's canonical 64-bit form (which is stronger than
/// Apple's 32-bit guarantee), and re-extending an already-extended value is a
/// no-op. One rule for every row is both correct and cheaper than a per-row
/// prologue.
fn arg_extensions(target: Target, param_types: &[Type]) -> Vec<ScalarAbiExtension> {
    let abi = TargetCCallAbi::for_target(target);
    param_types
        .iter()
        .map(|ty| abi.scalar_arg_extension(*ty))
        .collect()
}

// ============================================================================
// x86-64 / SysV AMD64
// ============================================================================

/// SysV integer argument registers, in order, as `(modrm_code, needs_rex_ext)`.
/// `rdi, rsi, rdx, rcx, r8, r9`.
const X86_ARG_REGS: [(u8, bool); 6] = [
    (7, false), // rdi
    (6, false), // rsi
    (2, false), // rdx
    (1, false), // rcx
    (0, true),  // r8
    (1, true),  // r9
];

fn generate_x86_64(target: Target, native_symbol: &str, param_types: &[Type]) -> MachineCode {
    let extensions = arg_extensions(target, param_types);
    let mut code = Vec::new();

    // Re-extend each narrow argument register in place before forwarding.
    for (index, ext) in extensions.iter().enumerate() {
        let (reg, ext_bit) = X86_ARG_REGS[index];
        x86_extend_reg(&mut code, reg, ext_bit, *ext);
    }

    // Framed forward to the native body. On entry `rsp % 16 == 8` (the C caller's
    // `call` pushed the return address); subtract 8 so the `call` below leaves
    // the native body's entry 16-byte aligned, then restore and return. The
    // native scalar/pointer return already sits in `rax` in the canonical form a
    // C caller accepts, so no epilogue fix-up is needed.
    code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x08]); // sub rsp, 8
    code.push(0xE8); // call rel32
    let reloc_offset = code.len() as u64;
    code.extend_from_slice(&[0, 0, 0, 0]); // rel32 placeholder
    let relocations = vec![EmittedRelocation::x86_call(reloc_offset, native_symbol)];
    code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8
    code.push(0xC3); // ret

    MachineCode {
        code,
        relocations,
        strings: Vec::new(),
    }
}

/// Emit an in-place canonical extension of the register `(code, ext_bit)`.
fn x86_extend_reg(code: &mut Vec<u8>, reg: u8, ext_bit: bool, ext: ScalarAbiExtension) {
    // (opcode bytes, REX.W set?) for a reg←reg extension.
    let (opcode, rex_w): (&[u8], bool) = match ext {
        ScalarAbiExtension::None => return,
        ScalarAbiExtension::Signed { from_bits: 8 } => (&[0x0F, 0xBE], true), // movsx r64, r8
        ScalarAbiExtension::Signed { from_bits: 16 } => (&[0x0F, 0xBF], true), // movsx r64, r16
        ScalarAbiExtension::Signed { from_bits: 32 } => (&[0x63], true),      // movsxd r64, r32
        ScalarAbiExtension::Unsigned { from_bits: 8 } => (&[0x0F, 0xB6], true), // movzx r64, r8
        ScalarAbiExtension::Unsigned { from_bits: 16 } => (&[0x0F, 0xB7], true), // movzx r64, r16
        ScalarAbiExtension::Unsigned { from_bits: 32 } => (&[0x8B], false), // mov r32, r32 (zero-extends)
        other => panic!("unexpected scalar extension for a target-C export argument: {other:?}"),
    };
    // REX: W from the operation, R and B both track the single register's
    // extension bit (destination is the ModRM reg field, source the rm field,
    // and both name the same physical register).
    let mut rex = 0x40;
    if rex_w {
        rex |= 0x08;
    }
    if ext_bit {
        rex |= 0x04 | 0x01;
    }
    if rex_w || ext_bit {
        code.push(rex);
    }
    code.extend_from_slice(opcode);
    code.push(0xC0 | (reg << 3) | reg); // ModRM: reg←reg
}

// ============================================================================
// AArch64 / AAPCS64
// ============================================================================

fn generate_aarch64(target: Target, native_symbol: &str, param_types: &[Type]) -> MachineCode {
    let extensions = arg_extensions(target, param_types);
    let mut words: Vec<u32> = Vec::new();

    for (index, ext) in extensions.iter().enumerate() {
        let reg = index as u32; // x0..x7 in order
        if let Some(word) = aarch64_extend_reg(reg, *ext) {
            words.push(word);
        }
    }

    // Framed forward to the native body: save FP/LR (keeping the 16-byte stack
    // alignment), branch-with-link to the native body, restore, and return. The
    // native scalar/pointer return already sits in `x0` canonically.
    words.push(0xA9BF_7BFD); // stp x29, x30, [sp, #-16]!
    words.push(0x9100_03FD); // mov x29, sp
    let bl_offset = (words.len() * 4) as u64;
    words.push(0x9400_0000); // bl <native>  (imm26 filled by relocation)
    words.push(0xA8C1_7BFD); // ldp x29, x30, [sp], #16
    words.push(0xD65F_03C0); // ret

    let mut code = Vec::with_capacity(words.len() * 4);
    for word in words {
        code.extend_from_slice(&word.to_le_bytes());
    }
    let relocations = vec![EmittedRelocation::aarch64_call(bl_offset, native_symbol)];

    MachineCode {
        code,
        relocations,
        strings: Vec::new(),
    }
}

/// The 32-bit instruction that canonically extends `x{reg}`/`w{reg}` in place,
/// or `None` when the value already fills its register.
fn aarch64_extend_reg(reg: u32, ext: ScalarAbiExtension) -> Option<u32> {
    let rd_rn = |base: u32| base | (reg << 5) | reg;
    Some(match ext {
        ScalarAbiExtension::None => return None,
        ScalarAbiExtension::Signed { from_bits: 8 } => rd_rn(0x9340_1C00), // sxtb x, w
        ScalarAbiExtension::Signed { from_bits: 16 } => rd_rn(0x9340_3C00), // sxth x, w
        ScalarAbiExtension::Signed { from_bits: 32 } => rd_rn(0x9340_7C00), // sxtw x, w
        ScalarAbiExtension::Unsigned { from_bits: 8 } => rd_rn(0x5300_1C00), // uxtb w, w
        ScalarAbiExtension::Unsigned { from_bits: 16 } => rd_rn(0x5300_3C00), // uxth w, w
        // uxtw: mov w, w (ORR Wd, WZR, Wm) zero-extends into the 64-bit register.
        ScalarAbiExtension::Unsigned { from_bits: 32 } => 0x2A00_03E0 | (reg << 16) | reg,
        other => panic!("unexpected scalar extension for a target-C export argument: {other:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RelocationKind;

    #[test]
    fn x86_scalar_passthrough_forwards_with_plt32_call() {
        // A u64 + i64 signature (both register-width) needs no extension: just
        // the framed call. Pointers classify identically (`None`).
        let code = generate_export_thunk(
            Target::X86_64Linux,
            "__rue_sem_native",
            &[Type::U64, Type::I64],
        );
        assert_eq!(code.relocations.len(), 1);
        assert_eq!(code.relocations[0].kind, RelocationKind::X86Plt32);
        assert_eq!(code.relocations[0].symbol, "__rue_sem_native");
        // sub rsp,8 / call rel32 / add rsp,8 / ret with a 4-byte displacement.
        assert_eq!(&code.code[..4], &[0x48, 0x83, 0xEC, 0x08]);
        assert_eq!(code.code[4], 0xE8);
        assert_eq!(
            &code.code[code.code.len() - 5..],
            &[0x48, 0x83, 0xC4, 0x08, 0xC3]
        );
    }

    #[test]
    fn x86_narrow_args_are_extended_in_place() {
        // i32 (arg0=rdi) sign-extends via movsxd; bool (arg1=rsi) zero-extends
        // from its byte via movzx.
        let code = generate_export_thunk(Target::X86_64Linux, "native", &[Type::I32, Type::BOOL]);
        // movsxd rdi, edi = 48 63 FF ; movzx rsi, sil = 48 0F B6 F6
        assert_eq!(&code.code[..3], &[0x48, 0x63, 0xFF]);
        assert_eq!(&code.code[3..7], &[0x48, 0x0F, 0xB6, 0xF6]);
    }

    #[test]
    fn aarch64_scalar_passthrough_forwards_with_call26() {
        let code = generate_export_thunk(Target::Aarch64Linux, "native", &[Type::I64, Type::U64]);
        assert_eq!(code.relocations.len(), 1);
        assert_eq!(code.relocations[0].kind, RelocationKind::Aarch64Call26);
        assert_eq!(code.relocations[0].symbol, "native");
        // No extension instructions, so the whole body is the 5-word frame.
        assert_eq!(code.code.len(), 5 * 4);
        assert_eq!(&code.code[..4], &0xA9BF_7BFDu32.to_le_bytes());
        assert_eq!(
            &code.code[code.code.len() - 4..],
            &0xD65F_03C0u32.to_le_bytes()
        );
    }

    #[test]
    fn aarch64_narrow_args_are_extended_in_place() {
        // i16 (x0) -> sxth x0,w0 ; u8 (x1) -> uxtb w1,w1
        let code = generate_export_thunk(Target::Aarch64Linux, "native", &[Type::I16, Type::U8]);
        assert_eq!(&code.code[..4], &(0x9340_3C00u32).to_le_bytes()); // sxth x0, w0
        assert_eq!(&code.code[4..8], &(0x5300_1C21u32).to_le_bytes()); // uxtb w1, w1
    }

    #[test]
    fn every_target_row_re_extends_narrow_export_arguments() {
        // The thunk asks the convention, not the architecture, which extension
        // each argument needs. Apple's row requires the caller to extend below
        // 32 bits, but the native body needs the canonical 64-bit form, so the
        // Darwin thunk re-extends exactly as the Linux one does.
        let params = [Type::I8, Type::U16, Type::I32, Type::BOOL, Type::I64];
        let linux = generate_export_thunk(Target::Aarch64Linux, "native", &params);
        let darwin = generate_export_thunk(Target::Aarch64Macos, "native", &params);
        assert_eq!(linux.code, darwin.code);
        // Four narrow arguments plus the five-word frame; `i64` needs nothing.
        assert_eq!(darwin.code.len(), 9 * 4);
    }
}
