// High-level code generation using HIR (High-level Intermediate Representation)
//
// This crate handles code generation from HIR to executable binaries.
// The old AST-based code generation has been removed in favor of HIR-based compilation.

use std::collections::HashMap;

// Re-export HIR compilation functions - this is the current API
mod compile_hir;
mod compile_hir_with_mir;
mod elf_writer;
mod hir_codegen;
mod label_utils;
mod lowering;
mod mir_to_instructions;
mod regalloc;
mod util;
mod x86_emitter;

pub use compile_hir::{compile_hir_to_assembly, compile_hir_to_executable};
pub use compile_hir_with_mir::{
    compile_hir_via_mir_to_assembly, compile_hir_via_mir_to_executable,
};
// ElfWriter is used internally by compile_hir module
pub use lowering::Lowering;
pub use regalloc::RegisterAllocator;
pub use x86_emitter::X86Emitter;

// Import types from rue-ir
use rue_ir::target::{LabelRef, MachineInstr, Register};

#[derive(Debug, Clone, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

/// Virtual register - will be allocated to a physical register or stack slot
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VReg(pub u32);

/// Value operand for instructions
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    VReg(VReg),
    Immediate(i64),
    PhysicalReg(Register),
}

/// Binary operations
#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Label for control flow jumps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(pub u32);

/// Platform-independent instruction set
///
/// Examples:
/// - `2 + 3` generates: Copy{v0, Imm(2)}, Copy{v1, Imm(3)}, BinaryOp{v2, v0, v1, Add}
/// - `x = 42` generates: Copy{v0, Imm(42)}, then maps variable "x" to v0
/// - `n * factorial(n-1)` generates: Push{v0}, Call{v1, "factorial", [v2]}, Pop{v3}, BinaryOp{v4, v3, v1, Mul}
#[derive(Debug, Clone)]
pub enum Instruction {
    // Data movement
    Copy {
        dest: VReg,
        src: Value,
    },

    // Arithmetic and comparison operations
    BinaryOp {
        dest: VReg,
        lhs: Value,
        rhs: Value,
        op: BinOp,
    },

    // Memory operations
    Load {
        dest: VReg,
        offset: i64,
    }, // Load from stack
    Store {
        src: VReg,
        offset: i64,
    }, // Store to stack

    // Stack operations for value preservation
    Push {
        src: VReg,
    }, // Push register to stack
    Pop {
        dest: VReg,
    }, // Pop from stack to register

    // Control flow
    Label(LabelId),
    Jump(LabelId),
    Branch {
        condition: VReg,
        true_label: LabelId,
        false_label: LabelId,
    },

    // Function operations
    Call {
        dest: Option<VReg>,
        function: String,
        args: Vec<VReg>,
    },
    Return {
        value: Option<VReg>,
    },

    // System operations
    Syscall {
        result: VReg,
        syscall_num: VReg,
        args: Vec<VReg>,
    },

    // Register preservation for calling convention
    SaveRegisters {
        registers: Vec<Register>,
    },
    RestoreRegisters {
        registers: Vec<Register>,
    },

    // Stack frame management
    EnterFrame,
    LeaveFrame,
}

// Convert machine instructions to AT&T assembly syntax
pub fn format_instructions_as_assembly(
    instructions: &[MachineInstr],
    function_labels: &HashMap<String, LabelId>,
) -> String {
    use rue_ir::target::ConditionCode;

    let mut output = String::new();

    // Assembly header
    output.push_str(".section .text\n");
    output.push_str(".global _start\n\n");

    // Reverse map for labels
    let mut label_names: HashMap<u32, String> = HashMap::new();
    for (name, LabelId(id)) in function_labels {
        label_names.insert(*id, name.clone());
    }

    for instr in instructions {
        match instr {
            MachineInstr::Label { id } => {
                if let Some(name) = label_names.get(id) {
                    output.push_str(&format!("{name}:\n"));
                } else {
                    output.push_str(&format!(".L{id}:\n"));
                }
            }
            MachineInstr::MovRR { dest, src } => {
                output.push_str(&format!(
                    "    movq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::MovRI32 { dest, imm } => {
                output.push_str(&format!("    movl ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::MovRI64 { dest, imm } => {
                output.push_str(&format!("    movabsq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::MovRM { dest, base, offset } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movq (%{}), %{}\n",
                        reg_name(base),
                        reg_name(dest)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movq {}(%{}), %{}\n",
                        offset,
                        reg_name(base),
                        reg_name(dest)
                    ));
                }
            }
            MachineInstr::MovMR { base, offset, src } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movq %{}, (%{})\n",
                        reg_name(src),
                        reg_name(base)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movq %{}, {}(%{})\n",
                        reg_name(src),
                        offset,
                        reg_name(base)
                    ));
                }
            }
            MachineInstr::MovMR8 { base, offset, src } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movb %{}, (%{})\n",
                        reg_name_8(src),
                        reg_name(base)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movb %{}, {}(%{})\n",
                        reg_name_8(src),
                        offset,
                        reg_name(base)
                    ));
                }
            }
            MachineInstr::MovRM8 { dest, base, offset } => {
                if *offset == 0 {
                    output.push_str(&format!(
                        "    movb (%{}), %{}\n",
                        reg_name(base),
                        reg_name_8(dest)
                    ));
                } else {
                    output.push_str(&format!(
                        "    movb {}(%{}), %{}\n",
                        offset,
                        reg_name(base),
                        reg_name_8(dest)
                    ));
                }
            }
            MachineInstr::AddRR { dest, src } => {
                output.push_str(&format!(
                    "    addq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::AddRI { dest, imm } => {
                output.push_str(&format!("    addq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::SubRR { dest, src } => {
                output.push_str(&format!(
                    "    subq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::SubRI { dest, imm } => {
                output.push_str(&format!("    subq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::ImulRR { dest, src } => {
                output.push_str(&format!(
                    "    imulq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::ImulRI { dest, imm } => {
                output.push_str(&format!("    imulq ${}, %{}\n", imm, reg_name(dest)));
            }
            MachineInstr::Idiv { divisor } => {
                output.push_str(&format!("    idivq %{}\n", reg_name(divisor)));
            }
            MachineInstr::Cqo => {
                output.push_str("    cqo\n");
            }
            MachineInstr::AndRR { dest, src } => {
                output.push_str(&format!(
                    "    andq %{}, %{}\n",
                    reg_name(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::Shl { dest, count: _ } => {
                output.push_str(&format!("    shlq %cl, %{}\n", reg_name(dest)));
            }
            MachineInstr::Sar { dest, count: _ } => {
                output.push_str(&format!("    sarq %cl, %{}\n", reg_name(dest)));
            }
            MachineInstr::CmpRR { left, right } => {
                output.push_str(&format!(
                    "    cmpq %{}, %{}\n",
                    reg_name(right),
                    reg_name(left)
                ));
            }
            MachineInstr::CmpRI { reg, imm } => {
                output.push_str(&format!("    cmpq ${}, %{}\n", imm, reg_name(reg)));
            }
            MachineInstr::SetCC { cc, dest } => {
                let cc_str = match cc {
                    ConditionCode::Equal => "sete",
                    ConditionCode::NotEqual => "setne",
                    ConditionCode::Less => "setl",
                    ConditionCode::LessEqual => "setle",
                    ConditionCode::Greater => "setg",
                    ConditionCode::GreaterEqual => "setge",
                };
                output.push_str(&format!("    {} %{}\n", cc_str, reg_name_8(dest)));
            }
            MachineInstr::Movzx { dest, src } => {
                output.push_str(&format!(
                    "    movzbq %{}, %{}\n",
                    reg_name_8(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::Movsxd { dest, src } => {
                output.push_str(&format!(
                    "    movslq %{}, %{}\n",
                    reg_name_32(src),
                    reg_name(dest)
                ));
            }
            MachineInstr::Push { reg } => {
                output.push_str(&format!("    pushq %{}\n", reg_name(reg)));
            }
            MachineInstr::Pop { reg } => {
                output.push_str(&format!("    popq %{}\n", reg_name(reg)));
            }
            MachineInstr::Call { target } => {
                output.push_str(&format!("    call {target}\n"));
            }
            MachineInstr::Ret => {
                output.push_str("    ret\n");
            }
            MachineInstr::Jmp { target } => match target {
                LabelRef::Local(id) => {
                    if let Some(name) = label_names.get(id) {
                        output.push_str(&format!("    jmp {name}\n"));
                    } else {
                        output.push_str(&format!("    jmp .L{id}\n"));
                    }
                }
                LabelRef::Global(name) => {
                    output.push_str(&format!("    jmp {name}\n"));
                }
            },
            MachineInstr::JmpCC { cc, target } => {
                let cc_str = match cc {
                    ConditionCode::Equal => "je",
                    ConditionCode::NotEqual => "jne",
                    ConditionCode::Less => "jl",
                    ConditionCode::LessEqual => "jle",
                    ConditionCode::Greater => "jg",
                    ConditionCode::GreaterEqual => "jge",
                };
                match target {
                    LabelRef::Local(id) => {
                        if let Some(name) = label_names.get(id) {
                            output.push_str(&format!("    {cc_str} {name}\n"));
                        } else {
                            output.push_str(&format!("    {cc_str} .L{id}\n"));
                        }
                    }
                    LabelRef::Global(name) => {
                        output.push_str(&format!("    {cc_str} {name}\n"));
                    }
                }
            }
            MachineInstr::Syscall => {
                output.push_str("    syscall\n");
            }
            MachineInstr::AllocStack { size } => {
                output.push_str(&format!("    subq ${size}, %rsp\n"));
            }
            MachineInstr::LeaLabel { dest, label } => {
                output.push_str(&format!("    leaq {}(%rip), %{}\n", label, reg_name(dest)));
            }
            MachineInstr::Cld => {
                output.push_str("    cld\n");
            }
            MachineInstr::RepStosb => {
                output.push_str("    rep stosb\n");
            }
            MachineInstr::EnterFrame => {
                output.push_str("    pushq %rbp\n");
                output.push_str("    movq %rsp, %rbp\n");
            }
            MachineInstr::LeaveFrame => {
                output.push_str("    movq %rbp, %rsp\n");
                output.push_str("    popq %rbp\n");
            }
        }
    }

    output
}

// Helper functions for register names
fn reg_name(reg: &Register) -> &'static str {
    match reg {
        Register::Rax => "rax",
        Register::Rbx => "rbx",
        Register::Rcx => "rcx",
        Register::Rdx => "rdx",
        Register::Rsi => "rsi",
        Register::Rdi => "rdi",
        Register::Rbp => "rbp",
        Register::Rsp => "rsp",
        Register::R8 => "r8",
        Register::R9 => "r9",
        Register::R10 => "r10",
        Register::R11 => "r11",
        Register::R12 => "r12",
        Register::R13 => "r13",
        Register::R14 => "r14",
        Register::R15 => "r15",
    }
}

fn reg_name_8(reg: &Register) -> &'static str {
    match reg {
        Register::Rax => "al",
        Register::Rbx => "bl",
        Register::Rcx => "cl",
        Register::Rdx => "dl",
        Register::Rsi => "sil",
        Register::Rdi => "dil",
        Register::Rbp => "bpl",
        Register::Rsp => "spl",
        Register::R8 => "r8b",
        Register::R9 => "r9b",
        Register::R10 => "r10b",
        Register::R11 => "r11b",
        Register::R12 => "r12b",
        Register::R13 => "r13b",
        Register::R14 => "r14b",
        Register::R15 => "r15b",
    }
}

fn reg_name_32(reg: &Register) -> &'static str {
    match reg {
        Register::Rax => "eax",
        Register::Rbx => "ebx",
        Register::Rcx => "ecx",
        Register::Rdx => "edx",
        Register::Rsi => "esi",
        Register::Rdi => "edi",
        Register::Rbp => "ebp",
        Register::Rsp => "esp",
        Register::R8 => "r8d",
        Register::R9 => "r9d",
        Register::R10 => "r10d",
        Register::R11 => "r11d",
        Register::R12 => "r12d",
        Register::R13 => "r13d",
        Register::R14 => "r14d",
        Register::R15 => "r15d",
    }
}

#[cfg(test)]
mod hir_test;

#[cfg(test)]
mod mir_to_instructions_test;

#[cfg(test)]
mod mir_roundtrip_test;
